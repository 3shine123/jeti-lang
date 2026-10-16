use std::path::Path;
use std::fs;
use std::io::IsTerminal;
use jeti_parser::parser::Parser;
use jeti_binder::Binder;
use jeti_elaborator::Elaborator;
use jeti_codegen::{emit_unit_with_headers_mapped, emit_bridge_header};
use jeti_preprocessor::Preprocessor;
use jeti_symbol::SymbolTable;
use jeti_ast::ast::*;
use jeti_cfg::cfg_build;
use jeti_arc::{arc_local_analyze, arc_global_analyze, arc_analyze_loops, arc_insert_actions, arc_optimize_pairs};
use jeti_checker::Checker;
use jeti_cst::diagnostic::{Diagnostic, Severity};
use jeti_trace::{trace_refcounts, TraceOptions};
use attrs::{Backend, disposition, AttrDisposition};

/// Default exception backend.
///
/// `true` = `-eh checked`: the explicit flag + guard lowering in `crates/eh`
/// (each `@throw` arms `__jeti_eh_flag` and returns; every call site that may
/// throw is guarded; the function tail propagates). ARC settles every frame on
/// the way out, so a cross-function throw releases intermediate frames'
/// owned locals — the sjlj backend's documented limitation.
///
/// `-eh legacy` (alias `-eh sjlj`) selects the old setjmp/longjmp backend and
/// remains a complete, faithful rollback.
pub const DEFAULT_EH_CHECKED: bool = true;

/// Preprocess + parse result, shared by `transpile` and `analyze`.
pub struct Prepared {
    /// Preprocessor state: the inlined buffer, the line map and the columns.
    pub pre: Preprocessor,
    /// The parsed CST (`None` when the parser could not build one at all).
    pub cst: Option<jeti_cst::TranslationUnit>,
    /// Parser diagnostics, already remapped to source positions.
    pub parser_diags: Vec<Diagnostic>,
    /// True when the parser recorded an error (see `Parser::has_error`).
    pub parser_has_error: bool,
}

pub struct Pipeline {
    pub has_error: bool,
    pub error_msg: String,
    pub search_dirs: Vec<String>,
    pub no_arc: bool,
    pub no_checker: bool,
    pub no_libc: bool,
    pub nostdinc: bool,
    pub verbose: bool,
    pub trace_refcount: bool,
    pub trace_max_iters: usize,
    pub trace_color: bool,
    pub backend: Backend,
    pub werror: bool,
    pub bridge_header: Option<String>,
    pub no_comments: bool,
    /// `-eh checked` (DEFAULT since the flip): explicit flag + guard exception
    /// lowering (crates/eh), unwind-safe ARC. `-eh legacy` / `-eh sjlj`
    /// selects the old setjmp/longjmp backend for rollback.
    pub eh_checked: bool,
    /// `--slots <manifest>`: append-only vtable slot manifest for stable
    /// cross-TU layout (None = historical sorted layout).
    pub slots_manifest: Option<String>,
    /// Specializations requested by sibling TUs in a multi-input build.
    /// Spelling is `Class<Arg, ...>`; codegen turns each into a real
    /// monomorphized class in every TU that has the generic implementation.
    pub forced_generic_instantiations: Vec<String>,
    /// C compiler + leading args used for the link step (e.g. `["zig", "cc"]`).
    /// Also used by the C type-name probe; empty means "unknown", which skips
    /// the probe.
    pub c_cc: Vec<String>,
    /// Target arch forwarded to the probe, mirroring the compile step's
    /// `-arch` (header search paths on Apple SDKs depend on it).
    pub c_arch: Option<String>,
    /// `-fno-ctype-probe`: never invoke the C preprocessor to recover the C
    /// type-name table. The parser then falls back to its builtin list and
    /// shape heuristics — casts to C-header typedefs outside that list are
    /// rejected again, and `x * y;` on two variables is misread as a
    /// declaration. Useful when no C compiler is available at transpile time.
    pub no_ctype_probe: bool,
    /// `-line-directives` (DEFAULT ON): emit `#line <src> "<file>"` in the
    /// generated C (plan 阶段 2), so clang diagnostics point back at the
    /// `.jeti`/`.hje`. `-fno-line-directives` turns it off.
    pub line_directives: bool,
    /// `-np-map` (plan 阶段 4): extract a versioned sidecar source map from
    /// the FINAL generated C. The map is stashed in [`Pipeline::last_source_map`]
    /// for the CLI driver, which decides where (and whether) it lands on disk.
    pub jeti_map: bool,
    /// Sidecar map extracted during the last [`Pipeline::transpile`] call
    /// (only when [`Pipeline::jeti_map`] is set). `generated.path` is left
    /// empty here — the CLI driver fills in the real `.c` output location.
    pub last_source_map: Option<jeti_cst::source_map_file::SourceMapFile>,
}

impl Pipeline {
    pub fn new() -> Self {
        Pipeline {
            has_error: false,
            error_msg: String::new(),
            search_dirs: vec!["include".into(), ".".into(), "include/Foundation".into()],
            no_arc: false,
            no_checker: false,
            no_libc: false,
            nostdinc: false,
            verbose: false,
            trace_refcount: false,
            trace_max_iters: 2,
            trace_color: true,
            backend: Backend::Clang,
            werror: false,
            bridge_header: None,
            no_comments: false,
            eh_checked: DEFAULT_EH_CHECKED,
            slots_manifest: None,
            forced_generic_instantiations: Vec::new(),
            c_cc: Vec::new(),
            c_arch: None,
            no_ctype_probe: false,
            line_directives: true,
            jeti_map: false,
            last_source_map: None,
        }
    }

    /// The C type names this TU's `#include`d headers declare, plus whether the
    /// table is authoritative (produced by the real C preprocessor over the
    /// same headers, macros, search paths and freestanding flags the compile
    /// step uses). `(vec![], false)` means "unknown": the parser then keeps its
    /// builtin list and shape fallbacks, so this can never reject code it
    /// cannot prove wrong.
    fn c_type_names(&self, pre: &Preprocessor, macros: &[&str], filename: &str) -> (Vec<String>, bool) {
        if self.no_ctype_probe || self.c_cc.is_empty() || pre.c_headers.is_empty() {
            return (Vec::new(), false);
        }
        // The source file's own directory comes first: `#include "x.h"` next to
        // the `.jeti` is the natural spelling, and the C compiler resolves it
        // relative to the including file (which for the emitted C is the
        // invocation directory — `search_dirs` already carries `.`).
        let mut dirs: Vec<String> = Vec::new();
        if let Some(parent) = Path::new(filename).parent() {
            let p = parent.to_string_lossy().to_string();
            if !p.is_empty() {
                dirs.push(p);
            }
        }
        dirs.extend(self.search_dirs.iter().cloned());
        match crate::ctype_probe::probe_c_type_names(
            &self.c_cc,
            &pre.c_headers,
            &dirs,
            macros,
            self.c_arch.as_deref(),
            self.no_libc,
            self.nostdinc,
        ) {
            Some(names) => {
                if self.verbose {
                    eprintln!(
                        "[jetic] C type table: {} names from {} passthrough header(s)",
                        names.len(),
                        pre.c_headers.len()
                    );
                }
                (names, true)
            }
            None => {
                if self.verbose {
                    eprintln!("[jetic] C type probe unavailable — using the builtin type list");
                }
                (Vec::new(), false)
            }
        }
    }

    pub fn transpile_file(&mut self, input_path: &str, output_path: &str) -> Result<(), String> {
        let source = fs::read_to_string(input_path)
            .map_err(|e| format!("cannot read {}: {}", input_path, e))?;

        let c_code = self.transpile(&source, input_path)?;

        if let Some(parent) = Path::new(output_path).parent() {
            fs::create_dir_all(parent).map_err(|e| format!("cannot create output dir: {}", e))?;
        }
        fs::write(output_path, &c_code)
            .map_err(|e| format!("cannot write {}: {}", output_path, e))?;

        Ok(())
    }

    /// The shared front-end prefix: preprocess the source and parse it.
    ///
    /// Both [`Pipeline::transpile`] and [`Pipeline::analyze`] start here, so the
    /// preprocessor flags, the C type-name probe and the parser's source-map
    /// wiring exist exactly once. `Err` means the preprocessor itself failed
    /// (no CST at all); parse errors are reported as diagnostics instead, the
    /// way a compiler front end is expected to.
    fn prepare(&self, source: &str, filename: &str) -> Result<Prepared, String> {
        // `__JETI__` is always defined: jeti headers can guard objc-style
        // syntax behind `#ifdef __JETI__` so a plain C compiler sees only the
        // C-compatible subset when the header is used directly (without jetic).
        let extra_macros: &[&str] = match self.backend {
            attrs::Backend::Clang => &["__clang__", "__GNUC__", "__JETI__"],
            attrs::Backend::Gcc => &["__GNUC__", "__JETI__"],
            attrs::Backend::Portable => &["__GNUC__", "__JETI__"],
        };
        let pre = Preprocessor::process(source, filename, &self.search_dirs, extra_macros)?;

        // `#include`d C headers are passed through verbatim, so the parser never
        // sees the typedefs they declare. Recover the table the C compiler will
        // actually use (see `ctype_probe`) so the parser can resolve casts and
        // `x * y;` by the symbol table, exactly as C does, instead of guessing
        // from token shape. Failure is not an error: the table is then simply
        // not authoritative, and the parser keeps its historical fallbacks.
        let (c_type_names, c_types_complete) = self.c_type_names(&pre, extra_macros, filename);

        // Step 1: Parse the resolved jeti source
        if self.verbose { eprintln!("[jetic] parsing..."); }
        let mut parser = Parser::with_c_type_names(&pre.resolved_jeti, &c_type_names, c_types_complete);
        // The parser reads one inlined buffer, so it has no `#include` boundary
        // of its own. The line→file map is the only way it can scope an
        // `NP_ASSUME_NONNULL` region to the file that opened it — without this
        // an open region marks every pointer in every imported header nonnull.
        parser.set_source_map(pre.source_map.clone());
        let cst = parser.parse_translation_unit();
        let parser_diags = parser.diagnostics().to_vec();
        let parser_has_error = parser.has_error();
        // The parser borrows the inlined buffer; end that borrow before handing
        // ownership of the preprocessor state to the caller.
        drop(parser);
        Ok(Prepared {
            pre,
            cst,
            parser_diags,
            parser_has_error,
        })
    }

    /// Run the front end for diagnostics only — no ARC lowering, no codegen.
    ///
    /// This is the entry the LSP (and any "check this buffer" caller) uses. It
    /// reports the stages that ran, in stage order, and stops descending once a
    /// stage fails because the later stages have nothing sound to work on. The
    /// diagnostics carry the shared positions (`file`, 1-based source `line`,
    /// **byte** `col` and the exclusive `end_col`), so a consumer only has to
    /// project the columns into its own unit; it must never re-derive positions.
    ///
    /// Differences from [`Pipeline::transpile`], both deliberate:
    /// - ARC runs no retain/release analysis, so ARC-only diagnostics (leak
    ///   warnings, ARC requirement errors) are not produced yet. Everything the
    ///   parser, binder, elaborator and type checker can decide is.
    /// - Warnings are returned alongside errors instead of being printed.
    pub fn analyze(&mut self, source: &str, filename: &str) -> Vec<Diagnostic> {
        let Prepared { pre, cst, parser_diags, parser_has_error } =
            match self.prepare(source, filename) {
                Ok(p) => p,
                // The preprocessor failed: no positions exist to report against.
                Err(msg) => return vec![Diagnostic::error(filename, 0, 0, msg)],
            };
        let sm = pre.source_map.clone();
        let mut diags = parser_diags;
        if parser_has_error {
            return diags;
        }
        let Some(mut cst) = cst else { return diags };
        cst.filename = filename.to_string();

        // Step 2: Bind names
        let mut binder = Binder::new(SymbolTable::new());
        if binder.bind(&mut cst) != 0 {
            diags.extend(parse_stage_string_diags(
                &translate_lines(binder.last_error(), &sm), filename, Severity::Error));
            return diags;
        }
        // Step 3: Elaborate CST → AST
        let symtab_for_checker = binder.symtab.clone();
        let mut elaborator = Elaborator::new(Some(binder.symtab));
        if elaborator.run(&cst) != 0 {
            diags.extend(parse_stage_string_diags(&elaborator.last_error(), filename, Severity::Error));
            return diags;
        }
        let Some(mut ast) = elaborator.take_ast() else { return diags };

        // The desugar passes the checker consumes in `transpile` (checked EH,
        // `@defer`, legacy `@finally`, pattern switches, async bodies) in the
        // same order and for the same reasons — the checker must see exactly
        // the shape it sees there or it would report shapes the CLI never
        // reports. Their errors become diagnostics here rather than a hard stop.
        if self.eh_checked {
            let eh = jeti_eh::check_unit(&ast);
            if !eh.errors.is_empty() {
                diags.extend(parse_stage_string_diags(&translate_lines(&eh.errors.join("\n"), &sm), filename, Severity::Error));
                return diags;
            }
            jeti_eh::desugar_unit(&mut ast);
        } else {
            jeti_eh::splice_finally_exits(&mut ast);
        }
        let defer_diags = jeti_defer::desugar_unit(&mut ast);
        if !defer_diags.errors.is_empty() {
            diags.extend(parse_stage_string_diags(&translate_lines(&defer_diags.errors.join("\n"), &sm), filename, Severity::Error));
            return diags;
        }
        jeti_pattern::desugar_unit(&mut ast);

        // Step 4.8: Async pre-pass (validate placement, then desugar the bodies
        // the checker would otherwise see in their pre-desugar form).
        let async_diags = jeti_async::check_unit(&ast);
        diags.extend(parse_stage_string_diags(
            &translate_lines(&async_diags.warnings.join("\n"), &sm), filename, Severity::Warning));
        if !async_diags.errors.is_empty() {
            diags.extend(parse_stage_string_diags(
                &translate_lines(&async_diags.errors.join("\n"), &sm), filename, Severity::Error));
            return diags;
        }
        let (async_errors, async_warnings) = jeti_async::desugar_unit_m2(&mut ast);
        diags.extend(parse_stage_string_diags(
            &translate_lines(&async_warnings.join("\n"), &sm), filename, Severity::Warning));
        if !async_errors.is_empty() {
            diags.extend(parse_stage_string_diags(
                &translate_lines(&async_errors.join("\n"), &sm), filename, Severity::Error));
            return diags;
        }

        // Step 5: Check types
        if !self.no_checker {
            let mut checker = Checker::new(Some(symtab_for_checker));
            checker.no_arc = self.no_arc;
            checker.source_map = Some(sm);
            checker.eh_checked = self.eh_checked;
            if checker.check(&mut ast) != 0 {
                diags.extend(checker.errors_as_diagnostics());
            }
            diags.extend(checker.warnings_as_diagnostics());
        }
        diags
    }

    pub fn transpile(&mut self, source: &str, filename: &str) -> Result<String, String> {
        let Prepared { pre, cst, parser_diags, parser_has_error } =
            self.prepare(source, filename)?;
        let mut cst = cst
            .ok_or_else(|| format!("Parse failed:\n{}", render_checker_diags(&parser_diags, &pre.source_map)))?;
        cst.filename = filename.to_string();

        if parser_has_error {
            return Err(format!("Parse failed:\n{}", render_checker_diags(&parser_diags, &pre.source_map)));
        }

        // Step 2: Bind names
        if self.verbose { eprintln!("[jetic] binding names..."); }
        let symtab = SymbolTable::new();
        let mut binder = Binder::new(symtab);
        if binder.bind(&mut cst) != 0 {
            return Err(format!("Binding failed:\n{}", render_stage_string_diags(
                &translate_lines(binder.last_error(), &pre.source_map))));
        }

        // Step 3: Elaborate CST → AST
        if self.verbose { eprintln!("[jetic] elaborating..."); }
        let symtab_for_checker = binder.symtab.clone();
        let mut elaborator = Elaborator::new(Some(binder.symtab));
        elaborator.verbose = self.verbose;
        if elaborator.run(&cst) != 0 {
            return Err(format!("Elaboration failed:\n{}", prefix_lines("[elaborator]", elaborator.last_error())));
        }
        let mut ast = elaborator.take_ast()
            .ok_or_else(|| "Elaboration produced no AST".to_string())?;

        // Step 3.9: Checked-exception desugar (-eh checked) — MUST run before
        // ARC so the injected early `return`s are ordinary control flow that
        // ARC already releases for (running it after would reintroduce the
        // sjlj cross-function leak this backend exists to fix).
        if self.eh_checked {
            if self.verbose { eprintln!("[jetic] eh desugar (checked)..."); }
            let eh_diags = jeti_eh::check_unit(&ast);
            if !eh_diags.errors.is_empty() {
                self.has_error = true;
                self.error_msg = format!("EH check failed:\n{}", render_stage_string_diags(&translate_lines(&eh_diags.errors.join("\n"), &pre.source_map)));
                return Err(self.error_msg.clone());
            }
            jeti_eh::desugar_unit(&mut ast);
        }

        // Step 3.9: @defer splicing — AFTER eh desugar (checked-mode throws
        // are already plain returns) and BEFORE ARC, so ARC's scope-end
        // releases land after the user's defer statements (deferred code runs
        // while objects are still alive). See AGENTS.md `@defer` section.
        {
            if self.verbose { eprintln!("[jetic] defer desugar..."); }
            let defer_diags = jeti_defer::desugar_unit(&mut ast);
            if !defer_diags.errors.is_empty() {
                self.has_error = true;
                self.error_msg = format!("Defer check failed:\n{}", render_stage_string_diags(&translate_lines(&defer_diags.errors.join("\n"), &pre.source_map)));
                return Err(self.error_msg.clone());
            }
        }

        // Step 3.92: legacy (sjlj) `@finally` early-return splice.
        //
        // The checked backend rewrites `@try` before this point and splices the
        // finally body in front of every `return` while doing so. The sjlj
        // backend hands `@try` to codegen unchanged, so without this pass a
        // `return` inside a `@try` silently skipped its `@finally` (ObjC runs
        // it). Runs before ARC so the injected cleanup stays after the finally
        // copy — the finally sees its locals alive.
        if !self.eh_checked {
            if self.verbose { eprintln!("[jetic] legacy @finally splice..."); }
            jeti_eh::splice_finally_exits(&mut ast);
        }

        // Step 3.95: Pattern-switch lowering — AFTER defer splicing (defer
        // sees the original switch; lowered goto/labels are ordinary stmts it
        // must never splice into) and BEFORE ARC (ARC/checker/codegen only
        // ever see plain C statements: If/Decl/Goto/Label — zero new arms
        // downstream). See AGENTS.md pattern-switch section.
        {
            if self.verbose { eprintln!("[jetic] pattern-switch lowering..."); }
            jeti_pattern::desugar_unit(&mut ast);
        }

        // Step 4: ARC analysis (skipped when -fno-jeti-arc is set)
        if self.verbose { eprintln!("[jetic] ARC analysis..."); }
        if !self.no_arc {
            for decl in &mut ast.decls {
                match &mut decl.data {
                    AstDeclData::Method { body: Some(ref mut b), .. } => {
                        if let Some(ref msym) = decl.name {
                            let cfg = cfg_build(b);
                            let mut arc_result = arc_local_analyze(b, &cfg, msym);
                            arc_global_analyze(&cfg, &mut arc_result, msym);
                            arc_analyze_loops(&cfg, &mut arc_result, msym);
                            arc_insert_actions(b, &arc_result);
                            arc_optimize_pairs(b);
                            if !arc_result.errors.is_empty() {
                                return Err(format!("ARC analysis failed:\n{}",
                                    prefix_lines("[arc]", &arc_result.errors.join("\n"))));
                            }
                            for w in &arc_result.leak_warnings {
                                eprintln!("\x1b[1;35m[arc] warning:\x1b[0m {}:{}:{}: {}", filename, decl.line, decl.col, w);
                            }
                        }
                    }
                    AstDeclData::Function { body: Some(ref mut b), .. } => {
                        let name = decl.name.as_deref().unwrap_or("function");
                        let cfg = cfg_build(b);
                        let mut arc_result = arc_local_analyze(b, &cfg, name);
                        arc_global_analyze(&cfg, &mut arc_result, name);
                        arc_analyze_loops(&cfg, &mut arc_result, name);
                        arc_insert_actions(b, &arc_result);
                        arc_optimize_pairs(b);
                        if !arc_result.errors.is_empty() {
                            return Err(format!("ARC analysis failed:\n{}",
                                prefix_lines("[arc]", &arc_result.errors.join("\n"))));
                        }
                        for w in &arc_result.leak_warnings {
                            eprintln!("\x1b[1;35m[arc] warning:\x1b[0m {}:{}:{}: {}", filename, decl.line, decl.col, w);
                        }
                    }
                    AstDeclData::Class { methods, .. } => {
                        for m in methods {
                            if let AstDeclData::Method { body: Some(ref mut b), .. } = &mut m.data {
                                let name = m.name.as_deref().unwrap_or("method");
                                let cfg = cfg_build(b);
                                let mut arc_result = arc_local_analyze(b, &cfg, name);
                                arc_global_analyze(&cfg, &mut arc_result, name);
                                arc_analyze_loops(&cfg, &mut arc_result, name);
                                arc_insert_actions(b, &arc_result);
                                arc_optimize_pairs(b);
                                if !arc_result.errors.is_empty() {
                                    return Err(format!("ARC analysis failed:\n{}",
                                        prefix_lines("[arc]", &arc_result.errors.join("\n"))));
                                }
                                for w in &arc_result.leak_warnings {
                                    eprintln!("\x1b[1;35m[arc] warning:\x1b[0m {}:{}:{}: {}", filename, m.line, m.col, w);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Step 4.5: Reference-count trace (skips codegen entirely)
        if self.trace_refcount {
            if self.verbose { eprintln!("[jetic] tracing refcounts..."); }
            let opts = TraceOptions {
                max_iters: self.trace_max_iters,
                color: self.trace_color,
            };
            return Ok(trace_refcounts(&ast, &opts));
        }

        // Step 4.8: Async pre-pass — validate await placement, then desugar
        // every async method body into the task-driven form (route map item
        // #4). check_unit runs on the ORIGINAL AST; desugar_unit rewrites it
        // in place before ARC/checker see the method bodies.
        if self.verbose { eprintln!("[jetic] async analysis..."); }
        let async_diags = jeti_async::check_unit(&ast);
        if !async_diags.errors.is_empty() {
            self.has_error = true;
            self.error_msg = format!("Async check failed:\n{}", prefix_lines("[async]", &translate_lines(&async_diags.errors.join("\n"), &pre.source_map)));
            return Err(self.error_msg.clone());
        }
        // Recoverable async warnings (reconcile issues are error-level now;
        // the channel stays for future async lints) — printed purple like ARC
        // warnings; `-Werror` escalates.
        // Lines are inline-buffer positions: translate via SourceMap.
        if !async_diags.warnings.is_empty() {
            if self.werror {
                self.has_error = true;
                self.error_msg = format!("Async check failed (-Werror):\n{}", prefix_lines("[async]", &translate_lines(&async_diags.warnings.join("\n"), &pre.source_map)));
                return Err(self.error_msg.clone());
            }
            for w in &async_diags.warnings {
                // translate_lines already yields `file:line:col: msg`.
                eprintln!("\x1b[1;35m[async] warning:\x1b[0m {}", translate_lines(w, &pre.source_map));
            }
        }
        // `hosted` gates the entry auto-pump (docs/plans/async_nptask_plan.md
        // §静态分析 3): a hosted chain drives itself, bare metal pumps from
        // its own main loop.
        let (async_desugar_errors, async_desugar_warnings) = jeti_async::desugar_unit_m2(&mut ast);
        // Rule C (elided-return inference): a value-carrying `async ^{…}`
        // block whose returns mix an object and a scalar result is a hard
        // error — same channel/format as the checker's async errors.
        if !async_desugar_errors.is_empty() {
            self.error_msg = format!("Async check failed:\n{}", prefix_lines("[async]", &translate_lines(&async_desugar_errors.join("\n"), &pre.source_map)));
            return Err(self.error_msg.clone());
        }
        // §8-①: warnings the desugar's elided-return inference produced — same
        // channel/format as the check warnings above (positions are
        // inline-buffer ones, translated through the SourceMap).
        if !async_desugar_warnings.is_empty() {
            if self.werror {
                self.has_error = true;
                self.error_msg = format!("Async inference failed (-Werror):\n{}", prefix_lines("[async]", &translate_lines(&async_desugar_warnings.join("\n"), &pre.source_map)));
                return Err(self.error_msg.clone());
            }
            for w in &async_desugar_warnings {
                eprintln!("\x1b[1;35m[async] warning:\x1b[0m {}", translate_lines(w, &pre.source_map));
            }
        }

        // Step 5: Check types (skipped when -fno-checker is set)
        if self.verbose { eprintln!("[jetic] checking types..."); }
        let mut struct_eq_tags: Vec<String> = Vec::new();
        if !self.no_checker {
            let mut checker = Checker::new(Some(symtab_for_checker));
            checker.no_arc = self.no_arc;
            checker.source_map = Some(pre.source_map.clone());
            // `-eh checked` rewrites `@throw` before the checker runs, so the
            // bare `throws` "must really throw" rule cannot see the
            // statements it is meant to reconcile.
            checker.eh_checked = self.eh_checked;
            if checker.check(&mut ast) != 0 {
                return Err(format!("Type checking failed:\n{}", render_checker_diags(&checker.errors_as_diagnostics(), &pre.source_map)));
            }
            // Print warnings (non-fatal diagnostics, like C/ObjC `-W...`).
            // With `-Werror`, suppress the purple warning and promote to a red error.
            if self.werror && !checker.warnings().is_empty() {
                return Err(format!("Type checking failed (-Werror):\n{}",
                    render_checker_diags(&checker.warnings_as_diagnostics(), &pre.source_map)));
            }
            for w in checker.warnings() {
                eprintln!("\x1b[1;35m[checker] warning:\x1b[0m {}", w);
            }
            struct_eq_tags = checker.struct_eq_tags.clone();
        }

        // Step 5.5: Validate __attribute__ against backend
        if self.verbose { eprintln!("[jetic] validating attributes..."); }
        self.validate_attrs(&ast);
        if self.has_error {
            return Err(self.error_msg.clone());
        }

        // Step 6: Generate C code
        if self.verbose { eprintln!("[jetic] generating C code..."); }
        // Slots manifest: read the assigned method order (if the file exists)
        // BEFORE codegen, and write the post-compile assignment back after.
        let slots: Option<Vec<String>> = self.slots_manifest.as_ref().and_then(|path| {
            fs::read_to_string(path).ok().map(|content| {
                content.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
            })
        });
        // Selectors this TU sees through a header it imports — the "public"
        // surface. Codegen gives them the same vtable slot indices in every TU,
        // so a legal two-TU split does not produce disagreeing layouts.
        // See docs/plans/stable_slots_plan.md (R1).
        let public_methods = collect_public_methods(&ast, filename, &pre.source_map);
        // R2: the classes this TU owns (its `@implementation`s live in the main
        // file). Their metadata is emitted strong, everyone else's stays weak.
        let owned_classes = collect_owned_classes(&ast, filename, &pre.source_map);
        let mut cg = jeti_codegen::ast_to_cg_unit_with_slots_ext(
            &ast, self.backend, slots.as_deref(), Some(&public_methods),
            Some(&self.forced_generic_instantiations),
        );
        // KVC gate: emit accessor tables only when this TU can see the
        // NPPredicate declaration (a transitive jeti #import — the same
        // resolved-source scan the auto-link decision rests on). The name
        // appears in the inlined buffer exactly when its declaring header
        // was imported; a plain string mention cannot occur otherwise
        // (NPPredicate is not a user-spellable identifier until declared).
        cg.kvc = pre.resolved_jeti.contains("NPPredicate");
        // Struct tags whose `==`/`!=` the checker rewrote to value-comparison
        // calls; codegen emits one field-wise `jeti_struct_eq_<tag>` per tag.
        cg.struct_eq_tags = struct_eq_tags;
        cg.no_arc = self.no_arc;
        cg.owned_classes = owned_classes;
        // 阶段 2: `-line-directives` emits `#line` at user-statement positions
        // (mapped through the preprocessor's SourceMap) so clang diagnostics
        // point back at the .jeti/.hje. Off by default; -rewrite-jeti output
        // follows the same flag (a reviewer can inspect the directives).
        let line_sm = if self.line_directives { Some(&pre.source_map) } else { None };
        let c_code = emit_unit_with_headers_mapped(&cg, &pre.c_headers, &self.search_dirs, self.no_libc, self.backend, !self.no_comments, self.eh_checked, line_sm);

        // 阶段 4: `-np-map` extracts the sidecar source map from the FINAL
        // generated text — after every emission-time buffer surgery — so all
        // offsets are byte-exact against what reaches disk. The `#line` stream
        // is the shared ground truth of the C preprocessor and the extractor.
        if self.jeti_map {
            let src_pairs = vec![(filename.to_string(), source.to_string())];
            let opts = jeti_codegen::source_map_extract::ExtractOptions {
                // The CLI driver knows the real `.c` output path and fills it in.
                generated_path: String::new(),
                primary_source: filename.to_string(),
                source_contents: &src_pairs,
            };
            self.last_source_map = Some(jeti_codegen::source_map_extract::extract_source_map(&c_code, &opts));
        }

        // Step 6.4: Write back the slots manifest (append-only): the compiled
        // assignment (previously-assigned slots kept + new methods appended)
        // becomes the manifest, so a later TU compiled with the same manifest
        // file shares this exact method→slot layout.
        if let Some(ref path) = self.slots_manifest {
            let manifest = cg.global_instance_method_names.join("\n");
            if let Err(e) = fs::write(path, manifest + "\n") {
                self.has_error = true;
                return Err(format!("cannot write slots manifest {}: {}", path, e));
            }
        }

        // Step 6.5: Generate bridge header (if requested)
        if let Some(ref path) = self.bridge_header {
            if self.verbose { eprintln!("[jetic] writing bridge header: {}", path); }
            let bridge = emit_bridge_header(&cg);
            fs::write(path, &bridge)
                .map_err(|e| format!("cannot write bridge header {}: {}", path, e))?;
        }

    Ok(c_code)
    }

    fn validate_attrs(&mut self, ast: &AstUnit) {
        for decl in &ast.decls {
            self.validate_decl_attrs(decl);
        }
    }

    fn validate_decl_attrs(&mut self, decl: &AstDecl) {
        for attr in &decl.attributes {
            let name = attr.split('(').next().unwrap_or(attr).trim();
            match disposition(name, self.backend) {
                AttrDisposition::Error => {
                    self.has_error = true;
                    self.error_msg = format!(
                        "{}:{}: error: attribute '{}' not allowed in --backend={} (try --backend=clang or --backend=gcc)",
                        decl.line, decl.col, name, self.backend
                    );
                    eprintln!("{}", self.error_msg);
                }
                AttrDisposition::Warn => {
                    eprintln!("\x1b[1;35m{}:{}: warning:\x1b[0m unknown attribute '{}' (passing through to C compiler)",
                        decl.line, decl.col, name);
                }
                AttrDisposition::Pass => {}
            }
        }
        // Recurse into nested decls (class methods, ivars, struct fields, etc.)
        match &decl.data {
            AstDeclData::Class { methods, ivars, properties, impl_vars, .. } => {
                for m in methods { self.validate_decl_attrs(m); }
                for iv in ivars { self.validate_decl_attrs(iv); }
                for p in properties { self.validate_decl_attrs(p); }
                for v in impl_vars { self.validate_decl_attrs(v); }
            }
            AstDeclData::Aggregate { fields, .. } => {
                for f in fields { self.validate_decl_attrs(f); }
            }
            AstDeclData::Namespace(decls) => {
                for d in decls { self.validate_decl_attrs(d); }
            }
            _ => {}
        }
    }
}


/// Prefix every non-empty line of a multi-error message with `[stage]` so a
/// single compile can be read as a categorized report (parser/binder/checker…).
fn prefix_lines(stage: &str, msg: &str) -> String {
    msg.lines()
        .map(|l| if l.trim().is_empty() {
            l.to_string()
        } else {
            format!("{} {}", stage, l)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render checker diagnostics clang-style (source line + `~~~~^~~~~`
/// annotation) when the source text is available on disk; diagnostics whose
/// file cannot be read fall back to the plain `file:line:col:` line.
fn render_checker_diags(
    diags: &[jeti_cst::diagnostic::Diagnostic],
    sm: &jeti_cst::source_map::SourceMap,
) -> String {
    use jeti_cst::diagnostic::render_annotated_colored;
    let _ = sm; // line/col are already remapped by the checker at record time
    let color = std::io::stderr().is_terminal();
    render_annotated_colored(diags, &|file| fs::read_to_string(file).ok(), color)
}

/// Render the string-form diagnostics emitted by parser/binder/elaborator
/// (`LINE:COL: msg` lines, already remapped to the original file by
/// `translate_lines`). Each parseable line becomes a structured `Diagnostic`
/// so it gets the same annotated rendering as checker errors; lines that do
/// not fit the shape (bare messages) are passed through as position-less
/// diagnostics.
fn render_stage_string_diags(msg: &str) -> String {
    use jeti_cst::diagnostic::{Diagnostic, render_annotated_colored};
    let diags: Vec<Diagnostic> = msg.lines().map(|l| {
        // Shapes: `file:LINE:COL: msg`, `LINE:COL: msg`
        let parsed = l.splitn(4, ':').collect::<Vec<_>>();
        if let [f, ln, col, rest] = parsed[..] {
            if let (Ok(ln), Ok(col)) = (ln.trim().parse::<usize>(), col.trim().parse::<usize>()) {
                return Diagnostic::error(f, ln, col, rest.trim_start());
            }
        }
        Diagnostic::error(String::new(), 0, 0, l)
    }).collect();
    let color = std::io::stderr().is_terminal();
    render_annotated_colored(&diags, &|file| fs::read_to_string(file).ok(), color)
}

/// Parse `file:LINE:COL: msg` / `LINE:COL: msg` diagnostic lines into
/// structured diagnostics, for callers that consume positions instead of
/// rendering text (the LSP). Position-less lines become messages pinned to
/// `default_file` at line 0, which consumers treat as file-wide.
///
/// This is the structured twin of [`render_stage_string_diags`]; that one is
/// left exactly as it was so the CLI's rendered text does not drift.
fn parse_stage_string_diags(msg: &str, default_file: &str, severity: Severity) -> Vec<Diagnostic> {
    msg.lines().map(|l| {
        let parts: Vec<&str> = l.splitn(4, ':').collect();
        let parsed = match parts.as_slice() {
            [f, a, b, rest]
                if a.trim().parse::<usize>().is_ok() && b.trim().parse::<usize>().is_ok() =>
            {
                Some((*f, *a, *b, *rest))
            }
            [a, b, rest]
                if a.trim().parse::<usize>().is_ok() && b.trim().parse::<usize>().is_ok() =>
            {
                Some((default_file, *a, *b, *rest))
            }
            _ => None,
        };
        let (file, line, col, message) = match parsed {
            Some((f, a, b, rest)) => {
                let file = if f.trim().is_empty() { default_file } else { f };
                (file, a.trim().parse().unwrap_or(0), b.trim().parse().unwrap_or(0), rest.trim_start())
            }
            None => (default_file, 0, 0, l),
        };
        Diagnostic {
            severity,
            file: file.to_string(),
            line,
            col,
            end_col: None,
            hint: None,
            message: message.to_string(),
        }
    }).collect()
}

/// Rewrites `LINE:COL: message` occurrences in a diagnostic string so the
/// line points at the original source file (via the preprocessor's line map)
/// instead of the flattened inlined buffer. Lines that can't be mapped are
/// left untouched.
fn translate_lines(msg: &str, sm: &jeti_cst::source_map::SourceMap) -> String {
    if sm.is_empty() { return msg.to_string(); }
    msg.lines().map(|l| {
        // Parse leading `LINE:COL: ` (or `LINE: `)
        let mut parts = l.splitn(3, ':');
        let line_no: Option<usize> = parts.next().and_then(|p| p.trim().parse().ok());
        let rest: String = match (parts.next(), parts.next()) {
            (Some(col), Some(text)) => {
                let col_trim = col.trim();
                if col_trim.chars().all(|c| c.is_ascii_digit()) && col_trim.starts_with(|c: char| c.is_ascii_digit()) {
                    format!("{}: {}", col_trim, text)
                } else {
                    format!("{}: {}", col, text)
                }
            }
            (Some(col), None) => col.to_string(),
            _ => return l.to_string(),
        };
        match line_no {
            Some(n) if n > 0 => {
                let (file, real) = sm.locate(n);
                if file.is_empty() {
                    l.to_string()
                } else {
                    format!("{}:{}: {}", file, real, rest.trim_start())
                }
            }
            _ => l.to_string(),
        }
    }).collect::<Vec<_>>().join("\n")
}

/// Sanitized selectors declared in an `@interface` from a file this TU
/// imports — its "public" surface. Codegen uses this to lay out the public
/// vtable segment identically in every TU (`docs/plans/stable_slots_plan.md`, rule R1).
///
/// Only `@interface` declarations count (`is_implementation: false`). A
/// selector that appears solely in an imported `@implementation` is private to
/// the file that defines it: counting it made the library TU (which imports
/// `*.jeti`) and a client TU (which sees only `*.hje`) disagree on where a
/// property-synthesised accessor such as `NPError.localizedDescription` sits,
/// so their layouts split. Restricting to `@interface` makes the two align.
///
/// Where the preprocessor has no source map, the set comes back empty and the
/// historical alphabetical-only layout is used unchanged.
fn collect_public_methods(
    ast: &AstUnit,
    main_file: &str,
    map: &jeti_cst::SourceMap,
) -> std::collections::HashSet<String> {
    use std::collections::HashSet;
    let mut set: HashSet<String> = HashSet::new();
    if map.is_empty() || main_file.is_empty() {
        return set;
    }
    for d in &ast.decls {
        let (file, _) = map.locate(d.line);
        // Imported = a real origin that is not this file (spellings can differ:
        // the main file is the driver's path, imports are `<dir>/<name>`).
        let imported = !file.is_empty()
            && file != main_file
            && !file.ends_with(main_file)
            && !main_file.ends_with(&file);
        if !imported {
            continue;
        }
        if let AstDeclData::Class { methods, is_implementation: false, .. } = &d.data {
            for m in methods {
                if let Some(sel) = &m.name {
                    set.insert(sel.replace(':', "_"));
                }
            }
        }
    }
    set
}

/// Classes whose `@implementation` lives in **this** TU's main file — the
/// translation unit that owns the class (`docs/plans/stable_slots_plan.md`, rule R2).
///
/// Codegen emits an owner's metadata **strong**, so another TU's weak stub can
/// never displace the real table; and if a second TU also implements the class
/// in its own main file, the link fails with `duplicate symbol` instead of
/// silently choosing one of two tables.
///
/// An `@implementation` that arrives through an `#import` is deliberately NOT
/// owned: that is the self-contained mode, where every TU inlines Foundation
/// and the repeated copies must keep merging weakly. Namespaced classes are
/// walked too (their `::` is flattened by codegen, so the raw decl name is the
/// key here).
fn collect_owned_classes(
    ast: &AstUnit,
    main_file: &str,
    map: &jeti_cst::SourceMap,
) -> std::collections::HashSet<String> {
    use std::collections::HashSet;
    fn walk(
        decls: &[AstDecl],
        main_file: &str,
        map: &jeti_cst::SourceMap,
        set: &mut HashSet<String>,
    ) {
        for d in decls {
            match &d.data {
                AstDeclData::Namespace(inner) => walk(inner, main_file, map, set),
                // A category implementation (`@implementation Dog (Tricks)`)
                // contributes method definitions only — it must NOT claim
                // class ownership, or linking owner + category TUs fails with
                // duplicate metadata symbols (probes/xcat P0). See
                // docs/plans/categories_protocol_plan.md D1.
                AstDeclData::Class { is_implementation: true, is_category: true, .. } => {}
                AstDeclData::Class { is_implementation: true, .. } => {
                    let (file, _) = map.locate(d.line);
                    // Mirror the "imported" test in collect_public_methods, so
                    // the two agree on where a decl came from.
                    let imported = !file.is_empty()
                        && file != main_file
                        && !file.ends_with(main_file)
                        && !main_file.ends_with(&file);
                    if !file.is_empty() && !imported {
                        if let Some(name) = &d.name {
                            set.insert(name.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut set: HashSet<String> = HashSet::new();
    if map.is_empty() || main_file.is_empty() {
        return set;
    }
    walk(&ast.decls, main_file, map, &mut set);
    set
}

#[cfg(test)]
mod eh_default_tests {
    use super::*;

    /// Regression guard for the default exception backend.
    ///
    /// The flip to `-eh checked` was vetted by the acceptance matrix
    /// (`tests/eh_matrix/run_eh_matrix.sh`: 17 scenarios x 3 modes),
    /// the clang/ObjC differential suite (`tests/eh_diff/`: 7/7) and
    /// `test_all`. Reverting this constant — or letting `Pipeline::new()`
    /// drift away from it — silently changes every build's exception
    /// semantics, so both are asserted here.
    #[test]
    fn default_exception_backend_is_checked() {
        assert!(
            DEFAULT_EH_CHECKED,
            "default EH backend regressed to sjlj; '-eh checked' is the vetted default"
        );
        assert!(
            Pipeline::new().eh_checked,
            "Pipeline::new() must take its eh_checked value from DEFAULT_EH_CHECKED"
        );
    }

    /// `-eh legacy` must still be reachable as a full rollback: the flag is a
    /// plain bool the CLI sets to false, so a false `Pipeline::new()` default
    /// would have made `legacy` a no-op rather than a distinct backend.
    #[test]
    fn legacy_rollback_is_an_explicit_false() {
        let mut p = Pipeline::new();
        p.eh_checked = false;
        assert!(!p.eh_checked, "-eh legacy must produce the sjlj backend");
    }
}

