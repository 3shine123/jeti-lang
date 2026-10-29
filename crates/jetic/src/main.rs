use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Collect concrete generic spellings from source text for a multi-TU build.
/// This is deliberately conservative: only angle-bracket forms whose argument
/// list is not a single declaration parameter are forwarded. The parser/codegen
/// remains authoritative when turning a spelling into an AstType.
fn scan_generic_instantiations(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' { i += 1; continue; }
        let mut b = i;
        while b > 0 && (bytes[b - 1].is_ascii_alphanumeric() || bytes[b - 1] == b'_' || bytes[b - 1] == b':') { b -= 1; }
        if b == i { i += 1; continue; }
        let mut depth = 1usize;
        let mut j = i + 1;
        while j < bytes.len() && depth > 0 {
            if bytes[j] == b'<' { depth += 1; }
            else if bytes[j] == b'>' { depth -= 1; }
            j += 1;
        }
        if depth == 0 {
            let spelling = &source[b..j];
            let args = &source[i + 1..j - 1];
            let base_name = &source[b..i];
            let arg_trim = args.trim();
            let looks_like_param = !arg_trim.is_empty()
                && arg_trim.chars().next().map_or(false, |c| c.is_ascii_uppercase())
                && arg_trim.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            let concrete = base_name.chars().next().map_or(false, |c| c.is_ascii_uppercase())
                && !looks_like_param
                && (args.contains('*') || args.contains("::")
                    || args.chars().any(|c| c.is_ascii_lowercase()));
            if concrete && !out.iter().any(|s| s == spelling) { out.push(spelling.to_string()); }
            i = j;
        } else { break; }
    }
    out
}
/// Transpile-affecting flags forwarded to a per-TU `jetic` subprocess.
///
/// One process per translation unit is required: codegen's method-metadata
/// tables are process-global (`OnceLock`), so a second in-process transpile is
/// not possible. Link-only flags (-L/-l/-asm/-framework) are not needed by the
/// transpile step and are deliberately absent.
#[derive(Clone, Copy)]
struct TuPass<'a> {
    exe: &'a str,
    include_dirs: &'a [String],
    no_arc: bool,
    no_checker: bool,
    werror: bool,
    eh_checked: bool,
    slots_manifest: Option<&'a String>,
    slots_seed: Option<&'a String>,
    backend: Option<&'a String>,
    arch: Option<&'a String>,
    no_libc: bool,
    nostdinc: bool,
    no_comments: bool,
    verbose: bool,
}

impl TuPass<'_> {
    /// Transpile one TU in a child `jetic`, writing the generated C to `out_c`
    /// and this unit's specialization manifest to `out_demands`. Returns the
    /// manifest (empty when the child wrote none).
    ///
    /// `capture` swallows the child's stderr on success: used for the main
    /// TU's manifest-only pre-pass, whose diagnostics the in-process transpile
    /// reports anyway.
    /// Flags every child transpile shares: include paths plus the passes' own
    /// switches (ARC, checker, EH backend, slots manifest, target/backend).
    /// Extracted so the template-instantiation pass below cannot drift from the
    /// per-TU pass — a specialization compiled with different ABI switches than
    /// its siblings would be a silent layout disagreement.
    fn common_args(&self) -> Vec<String> {
        let mut cmd: Vec<String> = Vec::new();
        for d in self.include_dirs {
            cmd.push("-I".into());
            cmd.push(d.clone());
        }
        if self.no_arc { cmd.push("-fno-jeti-arc".into()); }
        if self.no_checker { cmd.push("-fno-checker".into()); }
        if self.werror { cmd.push("-Werror".into()); }
        cmd.push("-eh".into());
        cmd.push(if self.eh_checked { "checked".into() } else { "legacy".into() });
        if let Some(slots) = self.slots_manifest {
            cmd.push("--slots".into());
            cmd.push(slots.clone());
        }
        // A child TU (sibling or template instantiation) must lay its vtable
        // out on the same frozen prefix as its siblings, or the two would
        // disagree exactly like mismatched headers do.
        if let Some(seed) = self.slots_seed {
            cmd.push("--slots-seed".into());
            cmd.push(seed.clone());
        }
        if let Some(b) = self.backend {
            cmd.push("-backend".into());
            cmd.push(b.clone());
        }
        if let Some(a) = self.arch {
            cmd.push("-arch".into());
            cmd.push(a.clone());
        }
        if self.no_libc { cmd.push("-ffreestanding".into()); }
        if self.nostdinc { cmd.push("-nostdinc".into()); }
        if self.no_comments { cmd.push("-no-comments".into()); }
        if self.verbose { cmd.push("-v".into()); }
        cmd
    }

    fn run(&self, tu: &str, out_c: &str, out_demands: &str, forced: &[String], capture: bool) -> Vec<String> {
        let mut cmd: Vec<String> = vec![
            self.exe.to_string(),
            "-rewrite-jeti".into(), tu.to_string(),
            "-o".into(), out_c.to_string(),
            "--emit-generic-demands".into(), out_demands.to_string(),
        ];
        cmd.extend(self.common_args());
        for inst in forced {
            cmd.push("--generic-instantiation".into());
            cmd.push(inst.clone());
        }
        if self.verbose {
            eprintln!("[jetic] transpiling TU: {}", tu);
        }
        let mut child = Command::new(&cmd[0]);
        let running = child.args(&cmd[1..]);
        let ok = if capture {
            match running.output() {
                Ok(out) => {
                    if !out.status.success() {
                        // The child's diagnostics ARE the user's: forwarding the
                        // captured stderr is what makes a pre-pass failure look
                        // exactly like the in-process one.
                        let _ = std::io::stderr().write_all(&out.stderr);
                    }
                    out.status.success()
                }
                Err(e) => {
                    eprintln!("error: failed to spawn jetic for {}: {}", tu, e);
                    std::process::exit(1);
                }
            }
        } else {
            match running.status() {
                Ok(st) => st.success(),
                Err(e) => {
                    eprintln!("error: failed to spawn jetic for {}: {}", tu, e);
                    std::process::exit(1);
                }
            }
        };
        if !ok {
            eprintln!("error: transpiling {} failed", tu);
            std::process::exit(1);
        }
        fs::read_to_string(out_demands)
            .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
            .unwrap_or_default()
    }

    /// Compile ONE demanded specialization out of a template source that a
    /// precompiled library ships (manifest `template` line). `--instantiate-
    /// only` keeps this TU's metadata weak, so the archive remains the owner of
    /// the bare class and only the new specialization is contributed here.
    fn run_template(&self, wrapper: &str, out_c: &str, demand: &str, out_demands: &str) -> bool {
        let mut cmd: Vec<String> = vec![
            self.exe.to_string(),
            "-rewrite-jeti".into(), wrapper.to_string(),
            "-o".into(), out_c.to_string(),
            "--generic-instantiation".into(), demand.to_string(),
            "--emit-generic-demands".into(), out_demands.to_string(),
            "--instantiate-only".into(),
        ];
        cmd.extend(self.common_args());
        if self.verbose {
            eprintln!("[jetic] instantiating template specialization: {}", demand);
        }
        // The generated C for a specialization is machine-made glue, and the
        // library it came from is compiled `-w`. Keep its warnings out of the
        // user's build; a real failure still forwards the child's stderr.
        match Command::new(&cmd[0]).args(&cmd[1..]).output() {
            Ok(out) => {
                if !out.status.success() {
                    let _ = std::io::stderr().write_all(&out.stderr);
                }
                out.status.success()
            }
            Err(e) => {
                eprintln!("error: failed to spawn jetic for {}: {}", wrapper, e);
                std::process::exit(1);
            }
        }
    }
}
use clap::{Command as ClapCommand, Arg};
use clap_complete::{Shell, generate};
use jetic::pipeline::{Pipeline, DEFAULT_EH_CHECKED};
use attrs;

/// Locate the installed jetic bundle root (the directory containing `include/`).
///
/// Resolution order (first candidate containing `include/jeti/runtime.h` wins):
///   1. `$JETI_HOME` — explicit override (also works when installed anywhere)
///   2. the executable's own directory (bundle layout: `bin/jetic` + `include/`)
///   3. up to three parent directories (dev layout: `target/<profile>/jetic`)
///   4. the current directory (last-resort fallback)
///
/// This replaces the old hard-coded "three parents up" guess, which broke once
/// the install layout (e.g. on Windows) differed from `target/release/jetic`.
fn resolve_bundle_root() -> std::path::PathBuf {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(home) = env::var("JETI_HOME") {
        if !home.trim().is_empty() {
            candidates.push(std::path::PathBuf::from(home));
        }
    }
    if let Ok(exe) = env::current_exe() {
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        if let Some(dir) = exe.parent() {
            // Prefer the bundle root (`PREFIX`, where `bin/jetic` lives →
            // `PREFIX/include`) and the dev root (`target/<profile>/jetic` →
            // `project/include`) BEFORE the executable's own directory. The
            // latter may contain a *stale* `include/` copied by build.rs, which
            // must not shadow the source tree.
            let mut cur = dir.to_path_buf();
            for _ in 0..3 {
                match cur.parent() {
                    Some(p) => { cur = p.to_path_buf(); candidates.push(cur.clone()); }
                    None => break,
                }
            }
            candidates.push(dir.to_path_buf());
        }
    }
    candidates.push(std::path::PathBuf::from("."));
    for c in &candidates {
        if c.join("include").join("jeti").join("runtime.h").exists() {
            return c.clone();
        }
    }
    candidates.into_iter().next().unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// Pick the C compiler for the chosen codegen backend, as a command word list
/// (so multi-word compilers like `zig cc` work).
///
/// - `$JETI_CC` overrides everything (may contain spaces, e.g. `zig cc`).
/// - `-backend gcc` → `gcc`.
/// - Otherwise: on Windows the default is `zig cc` (a single self-contained
///   toolchain that bundles libc for `windows-gnu`); on other platforms the
///   default is `clang`.
fn select_c_compiler(backend: attrs::Backend) -> Vec<String> {
    if let Ok(cc) = env::var("JETI_CC") {
        let words: Vec<String> = cc.split_whitespace().map(|s| s.to_string()).collect();
        if !words.is_empty() {
            return words;
        }
    }
    match backend {
        attrs::Backend::Gcc => vec!["gcc".to_string()],
        _ => {
            if cfg!(windows) {
                vec!["zig".to_string(), "cc".to_string()]
            } else {
                vec!["clang".to_string()]
            }
        }
    }
}

/// clap Command describing the jetic CLI — used to generate shell completions.
///
/// NOTE: jetic's real CLI uses *single-dash* long flags (`-rewrite-jeti`,
/// `-fno-jeti-arc`, `-arch`), which clap would normally normalize to
/// `--rewrite-jeti` etc. We declare the clap options, then post-process the
/// generated script in `gen_completions` to emit the single-dash forms.
fn clap_command() -> ClapCommand {
    ClapCommand::new("jetic")
        .version(env!("JETI_VERSION"))
        .about("Jeti language compiler")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .disable_help_subcommand(true)
        .arg(Arg::new("rewrite-jeti").long("rewrite-jeti")
            .help("transpile to C only (no link)"))
        .arg(Arg::new("verbose").short('v').long("verbose")
            .help("show verbose transpilation info"))
        .arg(Arg::new("version").long("version")
            .help("print version and exit"))
        .arg(Arg::new("arc").long("fjeti-arc")
            .help("enable ARC (default)"))
        .arg(Arg::new("no-arc").long("fno-jeti-arc")
            .help("disable ARC (MRC)"))
        .arg(Arg::new("no-checker").long("fno-checker")
            .help("skip type checking"))
        .arg(Arg::new("eh").long("eh").value_name("CHECKED|SJLJ")
            .help("exception backend: checked (Swift-scheme, no cross-frame leak) or sjlj (setjmp/longjmp, default; 'legacy' is an alias)"))
        .arg(Arg::new("ffreestanding").long("ffreestanding")
            .help("bare-metal/freestanding output"))
        .arg(Arg::new("nostdinc").long("nostdinc")
            .help("pass -nostdinc to the C compiler (orthogonal; -ffreestanding does NOT imply it)"))
        .arg(Arg::new("no-comments").long("no-comments")
            .help("omit readability comments in generated C (default: on)"))
        .arg(Arg::new("trace-refcount").long("trace-refcount")
            .help("print a static reference-count trace (no codegen)"))
        .arg(Arg::new("trace-max-iters").long("trace-max-iters").value_name("N")
            .help("loop iterations simulated in the refcount trace (default 2)"))
        .arg(Arg::new("trace-no-color").long("trace-no-color")
            .help("disable colors in the refcount trace"))
        .arg(Arg::new("backend").long("backend").value_name("CLANG|PORTABLE|GCC")
            .help("C compiler backend (clang is the default)"))
        .arg(Arg::new("output").short('o').value_name("PATH")
            .help("output path (binary or .c)"))
        .arg(Arg::new("include").short('I').value_name("DIR")
            .help("add include dir").action(clap::ArgAction::Append))
        .arg(Arg::new("lib").short('L').value_name("DIR")
            .help("add lib dir").action(clap::ArgAction::Append))
        .arg(Arg::new("asm").short('S').long("asm").value_name("FILE")
            .help("link a real assembly file (repeatable)").action(clap::ArgAction::Append))
        .arg(Arg::new("arch").long("arch").value_name("TARGET")
            .help("target arch (e.g. -arch x86_64)"))
        .arg(Arg::new("gen-completions").long("gen-completions").value_name("SHELL")
            .help("generate shell completion script (bash|zsh|fish|powershell|elvish)"))
        .arg(Arg::new("input").value_name("INPUT")
            .num_args(1..)
            .help("input .jeti file(s): first = main TU; each extra .jeti is an \
                   additional TU compiled and linked in (compile mode); .o/.a \
                   are linked as-is"))
        .subcommand(ClapCommand::new("run")
            .about("compile + run, then delete binary")
            .disable_help_flag(true)
            .arg(Arg::new("input").value_name("INPUT").required(true))
            .arg(Arg::new("args").value_name("ARGS").num_args(0..).last(true)))
}

/// Map clap's double-dash normalization back to jetic's single-dash flags.
const DOUBLE_TO_SINGLE: &[(&str, &str)] = &[
    ("--rewrite-jeti", "-rewrite-jeti"),
    ("--fjeti-arc", "-fjeti-arc"),
    ("--fno-jeti-arc", "-fno-jeti-arc"),
    ("--fno-checker", "-fno-checker"),
    ("--eh", "-eh"),
    ("--ffreestanding", "-ffreestanding"),
    ("--nostdinc", "-nostdinc"),
    ("--no-comments", "-no-comments"),
    ("--trace-refcount", "-trace-refcount"),
    ("--trace-max-iters", "-trace-max-iters"),
    ("--trace-no-color", "-trace-no-color"),
    ("--backend", "-backend"),
    ("--arch", "-arch"),
    ("--asm", "-asm"),
    ("--output", "-o"),
];

/// -gen-completions <shell>: emit a completion script for the given shell.
fn gen_completions(shell: &str) {
    let shell = match shell {
        "bash" => Shell::Bash,
        "zsh" => Shell::Zsh,
        "fish" => Shell::Fish,
        "powershell" | "pwsh" => Shell::PowerShell,
        "elvish" => Shell::Elvish,
        other => {
            eprintln!("error: unsupported shell '{}' (try bash, zsh, fish, powershell, elvish)", other);
            std::process::exit(1);
        }
    };
    let mut cmd = clap_command();
    let mut buf: Vec<u8> = Vec::new();
    generate(shell, &mut cmd, "jetic", &mut buf);
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut out = text;
    for (from, to) in DOUBLE_TO_SINGLE {
        out = out.replace(from, to);
    }
    // Deduplicate consecutive identical lines (e.g. the `-o` spec left after
    // `--output` → `-o`, which clap emits once for the short and once for long).
    let mut dedup: Vec<&str> = Vec::new();
    for line in out.lines() {
        if dedup.last().map_or(true, |last| *last != line) {
            dedup.push(line);
        }
    }
    println!("{}", dedup.join("\n"));
}

fn find_libjeti(custom_libs: &[String]) -> Option<String> {
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let lib_path = exe_dir.join("libjeti.a");
            if lib_path.exists() {
                return Some(exe_dir.to_string_lossy().to_string());
            }
        }
    }
    let mut paths = vec![
        "builddir".to_string(),
        "/opt/jeti/lib".to_string(),
        "/usr/local/lib/jeti".to_string(),
    ];
    for p in custom_libs { paths.push(p.clone()); }
    for p in &paths {
        let libpath = format!("{}/libjeti.a", p);
        if std::path::Path::new(&libpath).exists() {
            return Some(p.clone());
        }
    }
    None
}

/// The shared-Foundation file name for this platform, if `dir` holds one.
///
/// NOT consulted by the default link path — see the KNOWN ISSUE note below.
/// The helper stays because the build script can produce the artifact and the
/// investigation is expected to resume.
///
/// KNOWN ISSUE (measured, 2026-10): linking a client against a shared
/// Foundation gives correct results for ordinary dispatch but breaks the
/// KVC-driven paths — `NPPredicate` filtering returns 0 matches even for pure
/// Foundation objects (`@[ @1, @2, @3 ]` filtered by `intValue > 1`). Cause: a
/// declaration-only client emits the non-owned classes' `NPClass` metadata as
/// ZEROED tentative definitions (their `.kvc_entries` is deliberately NULL —
/// see the "KVC table pointer — owner static metadata only" note in codegen).
/// Statically the archive's STRONG definition wins the weak merge, so the
/// client's objects carry the real tables; dynamically the client's own
/// definition is what its own objects point at, so the library's
/// `jeti_kvc_value` walk finds no table and returns NULL. Enabling this
/// therefore needs a metadata-interposition design first, not just a link
/// flag.
#[allow(dead_code)]
fn foundation_shared_name(dir: &str) -> Option<&'static str> {
    let names: &[&'static str] = if cfg!(target_os = "macos") {
        &["libjetifoundation.dylib"]
    } else if cfg!(target_os = "windows") {
        &["libjetifoundation.dll", "jetifoundation.dll"]
    } else {
        &["libjetifoundation.so"]
    };
    let d = std::path::Path::new(dir);
    names.iter().copied().find(|n| d.join(n).is_file())
}

/// Find a precompiled Foundation library, same policy as `find_libjeti`:
/// next to the binary first, then known install/dev locations, then the
/// user's `-L` dirs. Returns the DIRECTORY that holds `libjetifoundation.a`.
fn find_libjetifoundation(custom_libs: &[String]) -> Option<String> {
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let lib_path = exe_dir.join("libjetifoundation.a");
            if lib_path.exists() {
                return Some(exe_dir.to_string_lossy().to_string());
            }
            // In-repo dev layout: target/debug/jetic → target/foundation —
            // derived from the BINARY's location, not the process cwd, so
            // the auto-link holds no matter which directory jetic runs from
            // (harnesses and test runners cd into test directories).
            if let Some(repo_target) = exe_dir.parent() {
                let lib_path = repo_target.join("foundation/libjetifoundation.a");
                if lib_path.exists() {
                    return Some(lib_path.parent().unwrap().to_string_lossy().to_string());
                }
            }
        }
    }
    let mut paths = vec![
        "target/foundation".to_string(), // in-repo dev location (build-foundation-lib.sh default)
        "/opt/jeti/lib".to_string(),
        "/usr/local/lib/jeti".to_string(),
    ];
    for p in custom_libs { paths.push(p.clone()); }
    for p in &paths {
        let libpath = format!("{}/libjetifoundation.a", p);
        if std::path::Path::new(&libpath).exists() {
            return Some(p.clone());
        }
    }
    None
}

/// A `lib<name>.templates` manifest shipped beside a precompiled archive.
///
/// WHY THIS EXISTS
/// ---------------
/// A precompiled library carries only the specializations its own TUs demanded
/// when it was built. A client that spells a NEW one (`Box<MyType *>`) gets no
/// method body out of the archive: codegen can only emit an empty weak stub, so
/// the link succeeds and the first dispatch jumps to NULL. The manifest names
/// the template sources the library was built from, so the driver can compile
/// just the demanded specialization and link it in — the library keeps shipping
/// binaries, its templates stay the source of truth.
///
/// Format (line-oriented, `#` starts a comment):
///   root     <dir relative to this manifest, or absolute>
///   surface  <header imported first, relative to root>
///   template <Class> <TypeParam...> <source .jeti, relative to root>
///   defined  <flat>   (metadata symbol the archive already defines)
struct TemplateManifest {
    /// Directory every path in this manifest is relative to (already joined).
    root: std::path::PathBuf,
    /// Declaration header the library TU was compiled against. Importing the
    /// same one keeps this specialization's public vtable segment — and so its
    /// `__sig` — identical to the archive's TUs.
    surface: String,
    /// (class name, type parameters, source path relative to `root`).
    templates: Vec<(String, Vec<String>, String)>,
    /// Metadata flats the archive already defines; a demand matching one needs
    /// no instantiation (and would otherwise be a duplicate definition).
    defined: std::collections::HashSet<String>,
}

impl TemplateManifest {
    fn parse(path: &str) -> Option<TemplateManifest> {
        let text = fs::read_to_string(path).ok()?;
        let dir = Path::new(path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let mut root = dir.clone();
        let mut surface = String::new();
        let mut templates: Vec<(String, Vec<String>, String)> = Vec::new();
        let mut defined = std::collections::HashSet::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') { continue; }
            let mut it = line.split_whitespace();
            match it.next() {
                Some("root") => {
                    if let Some(p) = it.next() {
                        let p = Path::new(p);
                        root = if p.is_absolute() { p.to_path_buf() } else { dir.join(p) };
                    }
                }
                Some("surface") => { if let Some(p) = it.next() { surface = p.to_string(); } }
                Some("template") => {
                    let class = it.next()?.to_string();
                    let rest: Vec<&str> = it.collect();
                    // One type parameter and a source path is the minimum a
                    // usable entry can have (`template Box T Box.jeti`).
                    if rest.len() < 2 { continue; }
                    let src = rest[rest.len() - 1].to_string();
                    let params = rest[..rest.len() - 1].iter().map(|s| s.to_string()).collect();
                    templates.push((class, params, src));
                }
                Some("defined") => { if let Some(f) = it.next() { defined.insert(f.to_string()); } }
                _ => {}
            }
        }
        // No surface or no template entry: the file cannot drive an
        // instantiation, so treat it as absent instead of failing a build.
        if surface.is_empty() || templates.is_empty() { return None; }
        Some(TemplateManifest { root, surface, templates, defined })
    }

    fn template_for(&self, base: &str) -> Option<&(String, Vec<String>, String)> {
        self.templates.iter().find(|(c, _, _)| c == base)
    }
}

/// Every template manifest this link can use: an explicit `--template-manifest`
/// wins, otherwise `lib<name>.templates` next to each archive the link pulls in.
/// No manifest simply means "the library shipped binaries only" — exactly the
/// pre-#3 behaviour, no new failure mode.
fn find_template_manifests(
    explicit: Option<&String>,
    lib_dirs: &[String],
    libs: &[String],
) -> Vec<TemplateManifest> {
    if let Some(p) = explicit {
        return match TemplateManifest::parse(p) {
            Some(m) => vec![m],
            None => {
                eprintln!("error: --template-manifest {} is not a usable template manifest", p);
                std::process::exit(1);
            }
        };
    }
    let mut names: Vec<String> = libs.to_vec();
    // The Foundation archive is auto-linked for declaration-only clients, so
    // its manifest has to be discovered the same way (by lib name, not by the
    // user spelling `-ljetifoundation`).
    names.push("jetifoundation".to_string());
    names.sort();
    names.dedup();
    let mut dirs: Vec<String> = lib_dirs.to_vec();
    if let Some(fdir) = find_libjetifoundation(lib_dirs) { dirs.push(fdir); }
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for d in &dirs {
        for n in &names {
            let p = format!("{}/lib{}.templates", d, n);
            if !seen.insert(p.clone()) { continue; }
            if let Some(m) = TemplateManifest::parse(&p) { out.push(m); }
        }
    }
    out
}

/// Layout base shipped beside an archive (`lib<name>.slots`). A library that
/// pins its slot assignment freezes a prefix every client TU must reproduce:
/// without it, a client whose own imports add ANY selector can shift a library
/// class's slot indices and trip R3 at startup. Only consulted when the user
/// passed no `--slots`; never written back (the file belongs to the library).
fn find_slots_seed(lib_dirs: &[String], libs: &[String]) -> Option<String> {
    let mut names: Vec<String> = libs.to_vec();
    // Same reasoning as find_template_manifests: the Foundation archive is
    // auto-linked by name, so it is never spelled `-ljetifoundation`.
    names.push("jetifoundation".to_string());
    names.sort();
    names.dedup();
    let mut dirs: Vec<String> = lib_dirs.to_vec();
    if let Some(fdir) = find_libjetifoundation(lib_dirs) { dirs.push(fdir); }
    for d in &dirs {
        for n in &names {
            let p = format!("{}/lib{}.slots", d, n);
            if std::path::Path::new(&p).is_file() { return Some(p); }
        }
    }
    None
}


fn spelling_is_erased_template(spelling: &str, params: &[String]) -> bool {
    let Some(lt) = spelling.find('<') else { return true };
    let Some(gt) = spelling.rfind('>') else { return true };
    if gt < lt { return true; }
    let mut depth = 0usize;
    let mut cur = String::new();
    let mut args = Vec::new();
    for ch in spelling[lt + 1..gt].chars() {
        match ch {
            '<' => { depth += 1; cur.push(ch); }
            '>' => { depth -= 1; cur.push(ch); }
            ',' if depth == 0 => { args.push(std::mem::take(&mut cur)); }
            _ => cur.push(ch),
        }
    }
    args.push(cur);
    args.iter().all(|a| {
        let bare = a.trim().trim_end_matches('*').trim();
        params.iter().any(|p| p == bare)
    })
}

/// The signal that killed a child, if it was killed rather than exiting.
/// `ExitStatus::code()` is `None` in that case — which is how a crash used to
/// reach the user as a bare `1` with no explanation at all.
#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// Name for the signals a crashing program realistically dies of. Only the
/// numbers that agree across platforms are named (SIGBUS is 7 on Linux and 10
/// on Darwin, so it stays unnamed rather than lying on one of them); anything
/// else reports the bare number.
fn signal_name(n: i32) -> &'static str {
    match n {
        2 => "SIGINT",
        4 => "SIGILL",
        6 => "SIGABRT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        15 => "SIGTERM",
        _ => "signal",
    }
}

/// The C for a specialization is machine-made glue compiled for a concrete
/// type argument — the library it came from is built `-w` for exactly that
/// reason. Keep its diagnostics out of the user's build without silencing the
/// user's own translation units: the pragma is scoped to this one file and sits
/// above the inlined headers.
fn quiet_generated_glue(path: &str) {
    let Ok(src) = fs::read_to_string(path) else { return };
    if src.starts_with("/* jetic: generated glue") { return; }
    let guarded = "/* jetic: generated glue — see instantiate_shipped_templates */\n\
#if defined(__clang__)\n\
#pragma clang diagnostic ignored \"-Weverything\"\n\
#elif defined(__GNUC__)\n\
#pragma GCC diagnostic ignored \"-Wall\"\n\
#endif\n";
    let _ = fs::write(path, format!("{}{}", guarded, src));
}

/// Compile one specialization per satisfied demand straight out of the shipped
/// template sources, returning the generated C files for the link step.
///
/// Only spelling strings travel between processes; the type substitution stays
/// inside the compiler, so every specialization still has exactly one
/// authoritative definition (R2). Transitively demanded specializations are
/// followed through the child's own demand manifest, the same closure the
/// multi-TU driver runs for source TUs.
fn instantiate_shipped_templates(
    pass: &TuPass,
    manifests: &[TemplateManifest],
    demands: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut done: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut queue: Vec<String> = demands.iter().filter(|d| !d.trim().is_empty()).cloned().collect();
    queue.sort();
    queue.dedup();
    while let Some(demand) = queue.pop() {
        if !done.insert(demand.clone()) { continue; }
        let base = demand.split('<').next().unwrap_or(&demand).trim().to_string();
        let Some(m) = manifests.iter().find(|m| m.template_for(&base).is_some()) else { continue };
        let (_, params, rel_src) = m.template_for(&base).unwrap();
        // `Box<T>` inside the template body is the erased template, not a
        // specialization of it.
        if spelling_is_erased_template(&demand, params) { continue; }
        let flat = jeti_codegen::symbol_flat_name(&demand);
        // The archive already defines this one: instantiating it again would
        // collide with the library's own body.
        if m.defined.contains(&flat) { continue; }
        let src_path = m.root.join(rel_src);
        let body = match fs::read_to_string(&src_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!(
                    "error: template manifest names {} for {} but it cannot be read: {}",
                    src_path.display(), base, e
                );
                std::process::exit(1);
            }
        };
        // The wrapper mirrors how the library itself was built: the shared
        // declaration surface first, then the template source. That keeps this
        // TU's public vtable segment (and `__sig`) identical to the archive's.
        let tmp = std::env::temp_dir().join(format!(
            "jetic-inst-{}-{}", std::process::id(), flat
        ));
        let _ = fs::create_dir_all(&tmp);
        let wrapper = tmp.join(format!("{}.template.jeti", flat));
        let out_c = tmp.join(format!("{}.c", flat));
        let dem = tmp.join(format!("{}.demands", flat));
        let text = format!(
            "// generated: specialization of {} out of a shipped template source\n#import <{}>\n{}",
            base, m.surface, body
        );
        if let Err(e) = fs::write(&wrapper, text) {
            eprintln!("error: cannot write template wrapper {}: {}", wrapper.display(), e);
            std::process::exit(1);
        }
        // Include order matters: the manifest root (its `surface`) first, then
        // the library's own directory, then whatever the user passed.
        let mut incs: Vec<String> = vec![m.root.to_string_lossy().to_string()];
        let inner = m.root.join("Foundation");
        if inner.is_dir() { incs.push(inner.to_string_lossy().to_string()); }
        incs.extend(pass.include_dirs.iter().cloned());
        let tpass = TuPass { include_dirs: &incs, ..*pass };
        if pass.verbose {
            eprintln!("[jetic] template pass: {} → {}", demand, out_c.display());
        }
        if !tpass.run_template(&wrapper.to_string_lossy(), &out_c.to_string_lossy(), &demand, &dem.to_string_lossy()) {
            eprintln!("error: cannot instantiate {} from {}", demand, src_path.display());
            std::process::exit(1);
        }
        quiet_generated_glue(&out_c.to_string_lossy());
        // A cloned body may itself demand further specializations (the same
        // transitive chain the multi-TU closure follows between processes).
        if let Ok(ds) = fs::read_to_string(&dem) {
            for d in ds.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
                if !done.contains(d) { queue.push(d.to_string()); }
            }
        }
        out.push(out_c.to_string_lossy().to_string());
    }
    out
}

/// Extract the target of an `#import <...>` directive, if any.
fn import_angle_target(t: &str) -> Option<&str> {
    let start = t.find('<')?;
    let end = start + t[start..].find('>')?;
    Some(&t[start + 1..end])
}

/// Extract the target of an `#import "..."` directive, if any.
fn import_quoted_target(t: &str) -> Option<&str> {
    let start = t.find('"')?;
    let rest = &t[start + 1..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// True when this TU — or anything it transitively `#import`s — pulls in a
/// Foundation IMPLEMENTATION file (`*.jeti` under Foundation: the
/// self-contained umbrella `Foundation.jeti` or a per-class `NPString.jeti`).
/// Such a TU already carries the full Foundation code; auto-linking the
/// precompiled library would add a competing strong copy of the class
/// tables whose Foundation-only vtable sig wins the weak merge and trips
/// the cross-TU `__sig` check (tests/multi_tu/12_native_multi_input, and
/// cross_file_test.jeti whose Foundation arrives via `diamond_base.hje`).
/// Declaration-only clients (`Foundation.hje`) do not match and stay
/// auto-linkable — the ObjC library model.
///
/// Checked at the SOURCE level on purpose: generated-C text markers cannot
/// distinguish definitions from references (prototypes, vtable slots and
/// stub instances all mention `NP<Class>_*`). Resolution mirrors the
/// preprocessor's search roots (the importing file's directory, bundle
/// `include/` + `include/Foundation/`), and only imports resolving INTO the
/// real Foundation count — a user's own `X.jeti` or a shadowing directory is
/// ignored. Commented-out `#import` lines never match (they do not start
/// with `#import`); block comments are not handled (none in the bundled
/// headers).
fn tu_imports_foundation_impl(path: &str) -> bool {
    let bundle = resolve_bundle_root();
    let mut visited = std::collections::HashSet::new();
    foundation_impl_scan(std::path::Path::new(path), &bundle, &mut visited)
}

fn foundation_impl_scan(
    path: &std::path::Path,
    bundle: &std::path::Path,
    visited: &mut std::collections::HashSet<std::path::PathBuf>,
) -> bool {
    if !visited.insert(path.to_path_buf()) {
        return false;
    }
    let Ok(src) = fs::read_to_string(path) else { return false; };
    let file_dir = path.parent();
    for line in src.lines() {
        let t = line.trim_start();
        if !t.starts_with("#import") { continue; }
        let target = match import_angle_target(t).or_else(|| import_quoted_target(t)) {
            Some(x) => x,
            None => continue,
        };
        if !(target.ends_with(".hje") || target.ends_with(".jeti")) { continue; }
        // Resolution order mirrors the preprocessor: the importing file's
        // own directory first, then the bundle's include roots.
        let mut roots: Vec<std::path::PathBuf> = Vec::new();
        if let Some(d) = file_dir { roots.push(d.to_path_buf()); }
        roots.push(bundle.join("include"));
        roots.push(bundle.join("include").join("Foundation"));
        let Some(resolved) = roots.iter().map(|r| r.join(target)).find(|p| p.exists()) else { continue; };
        if target.ends_with(".jeti")
            && resolved.starts_with(bundle.join("include").join("Foundation"))
        {
            return true; // a Foundation implementation is inlined into this TU
        }
        if foundation_impl_scan(&resolved, bundle, visited) {
            return true;
        }
    }
    false
}

fn compile_to_binary(cc: &[String], c_code: &str, bin_path: &str, include_dirs: &[String], lib_dirs: &[String], libs: &[String], asm_files: &[String], extra_c_files: &[String], extra_objects: &[String], frameworks: &[String], arch: Option<&str>, verbose: bool, no_libc: bool, nostdinc: bool, shared: bool, foundation_impl_inlined: bool, debug_symbols: bool, opt_level: Option<&str>, dead_strip: bool, eh_checked: bool) {
    // Compiler may be multi-word (e.g. `zig cc`): program + leading args.
    let program = cc.first().map(|s| s.as_str()).unwrap_or("clang");
    let cc_extra: Vec<String> = cc.get(1..).unwrap_or(&[]).to_vec();
    if verbose { eprintln!("[jetic] compiling with {}...", cc.join(" ")); }
    let self_dir = resolve_bundle_root();
    let include_root = self_dir.join("include");
    let foundation_include = self_dir.join("include").join("Foundation");
    let runtime_c = self_dir.join("include").join("jeti").join("runtime.c");
    if verbose {
        eprintln!("[jetic]   self_dir={:?}", self_dir);
        eprintln!("[jetic]   runtime_c={:?}", runtime_c);
    }
    // `-x none` resets the language mode after the stdin code so the file
    // inputs below compile as their own languages. Only add it when something
    // actually follows: a trailing `-x none` with no input files makes clang
    // warn "'-x none' after last input file has no effect" — which real clang
    // backends surface in stderr diffs (the removed `-w` force-feed used to
    // silence even this).
    let has_file_inputs =
        !extra_c_files.is_empty() || !extra_objects.is_empty() || !asm_files.is_empty();
    let mut clang_args = vec!["-x".to_string(), "c".to_string(), "-".to_string()];
    if has_file_inputs {
        clang_args.push("-x".to_string());
        clang_args.push("none".to_string());
    }
    if shared {
        // `-shared` is a gcc/clang shared flag (Linux → .so). Darwin's clang
        // accepts it too, but `-dynamiclib` is the platform convention for a
        // Mach-O .dylib. Pick per-OS so both toolchains stay in their idiom.
        if std::env::consts::OS == "macos" {
            clang_args.push("-dynamiclib".to_string());
        } else {
            clang_args.push("-shared".to_string());
        }
    }
    if no_libc {
        // Bare-metal freestanding flags
        clang_args.push("-ffreestanding".to_string());
        clang_args.push("-fno-builtin".to_string());
        clang_args.push("-fno-stack-protector".to_string());
        clang_args.push("-fno-pic".to_string());
        clang_args.push("-fno-pie".to_string());
        clang_args.push("-fno-asynchronous-unwind-tables".to_string());
        clang_args.push("-nostdlib".to_string());
        if let Some(a) = arch {
            if a.contains("86") || a.contains("x86_64") {
                clang_args.push("-mno-sse".to_string());
                clang_args.push("-mno-mmx".to_string());
                clang_args.push("-mno-red-zone".to_string());
            }
        }
    }
    if nostdinc {
        // Orthogonal include-path control: strip the system include search
        // path only (no other bare-metal flags). Headers must come from the
        // user's -I dirs — note -nostdinc also removes the compiler's builtin
        // freestanding set, so stdint.h/stddef.h/stdbool.h must be supplied.
        clang_args.push("-nostdinc".to_string());
    }
    // Cross-target ARCH selection (e.g. `-arch x86_64` to build via Rosetta).
    if let Some(a) = arch {
        clang_args.push("-arch".to_string());
        clang_args.push(a.to_string());
    }
    // Optimization level. Left to the C compiler, the default is -O0, which
    // made every build ~2x slower than it needs to be: measured on
    // tests/jeti_foundation_bfs_benchmark, -O0 → -O2 is 1.48x on the
    // Foundation-container path and 2.08x on the raw C path, and -O2 → -O3
    // another 14–17% — at an unchanged binary size (the class metadata, not the
    // code, dominates the file). -O3 is also what the precompiled Foundation
    // library is built with (`FOUNDATION_CFLAGS` defaults to -O3), so the client
    // and the library now agree.
    //
    // Debug symbols (plan 阶段 3): `-g` keeps its historical `-g -O0` preset so
    // DWARF (via the `#line` directives when enabled) points back at a
    // debuggable .jeti/.hje frame. An explicit -O<level> overrides either
    // default — clang takes the last -O on the line, and this is that last one.
    if debug_symbols {
        clang_args.push("-g".to_string());
    }
    clang_args.push(match opt_level {
        Some(level) => level.to_string(),
        None if debug_symbols => "-O0".to_string(),
        // The sjlj (legacy) backend keeps its historical -O0 default: its
        // generated `setjmp`/`longjmp` state machine leaves locals that survive
        // a longjmp non-volatile, so the optimizer caches them in registers and
        // the handler reads stale values. Measured on tests/full_syntax_test.jeti
        // §4.8/4.9/4.10 (bare-metal and hosted alike): -O0 gives the documented
        // 211/11/11, while -O2/-O3 give 210/10/10 — silently wrong, no
        // diagnostic. `checked` (the default backend) has no such coupling and
        // keeps -O3. An explicit -O<level> still wins here.
        None if !eh_checked => "-O0".to_string(),
        None => "-O3".to_string(),
    });
    // Additional translation units (multi-input mode): plain file inputs on
    // the driver line — the C compiler compiles each TU and links everything
    // in the same invocation (same shape as the bundled runtime.c below).
    for c in extra_c_files {
        clang_args.push(c.clone());
    }
    // Precompiled objects/archives (positional extra inputs): linked as-is.
    for obj in extra_objects {
        clang_args.push(obj.clone());
    }
    // Real assembly (.s) / object (.o) files: assembled/linked alongside.
    for asm_file in asm_files {
        clang_args.push(asm_file.clone());
    }
    // Apple frameworks to link (e.g. `-framework Cocoa`): lets Jeti programs
    // link against ObjC bridges (or the runtime) via a thin C API.
    for fw in frameworks {
        clang_args.push("-framework".to_string());
        clang_args.push(fw.clone());
    }
    clang_args.push("-I".to_string());
    clang_args.push(include_root.to_string_lossy().to_string());
    clang_args.push("-I".to_string());
    clang_args.push(foundation_include.to_string_lossy().to_string());
    clang_args.push("-o".to_string());
    clang_args.push(bin_path.to_string());
    for d in include_dirs {
        clang_args.push("-I".to_string());
        clang_args.push(d.clone());
    }
    if no_libc {
        // Freestanding: user provides their own runtime (JETI_CLASS_$_jeti_root,
        // exception state, etc.).  Do NOT link the bundled runtime.c (which
        // depends on libc malloc, __thread, etc.).
        if verbose { eprintln!("[jetic]   bare-metal mode: skipping runtime.c"); }
    } else if shared {
        // Shared library: the runtime lives in the host executable — a module
        // must not carry its own copy (duplicate symbols / two metas). It talks
        // to the host through the ModAPI function-pointer table instead.
        if verbose { eprintln!("[jetic]   shared-library mode: skipping runtime.c"); }
    } else {
        // Link a prebuilt static libjeti if one is available; otherwise compile
        // the bundled runtime.c source. Never both (duplicate symbols).
        if let Some(lib_path) = find_libjeti(lib_dirs) {
            clang_args.push("-L".to_string());
            clang_args.push(lib_path);
            clang_args.push("-ljeti".to_string());
        } else {
            if !has_file_inputs {
                // Reset the language mode after the stdin code before the
                // runtime source (no other file inputs followed).
                clang_args.push("-x".to_string());
                clang_args.push("none".to_string());
            }
            clang_args.push(runtime_c.to_string_lossy().to_string());
        }
    }
    // Platform link conveniences (host mode only). Detected from the GENERATED
    // code, so the portable (gcc) backend — which expands blocks away — is
    // automatically exempt. Extra TUs count too: a block literal in any TU
    // needs the blocks runtime at link time.
    let extras_have_blocks = extra_c_files.iter()
        .filter_map(|p| fs::read_to_string(p).ok())
        .any(|s| s.contains("(^"));
    if !no_libc && (c_code.contains("(^") || extras_have_blocks) {
        // Blocks: Apple's clang enables them by default, upstream clang (Linux)
        // does not — and needs the separate BlocksRuntime at link time
        // (apt install libblocksruntime-dev).
        clang_args.push("-fblocks".to_string());
        if std::env::consts::OS == "linux" && !shared {
            clang_args.push("-lBlocksRuntime".to_string());
        }
    }
    if !no_libc && !shared && std::env::consts::OS == "linux" {
        // macOS folds libm into libSystem (sin/cos link implicitly); Linux
        // needs an explicit -lm. The linker drops it when unused (--as-needed).
        clang_args.push("-lm".to_string());
    }
    // Dead stripping: drop functions/data the program never reaches. Jeti's
    // link model pulls WHOLE archive members — a declaration-only client
    // references every class it sees, and each Foundation member's KVC wrapper
    // table references more classes — so a Foundation hello-world carries most
    // of Foundation. Measured: macOS `-dead_strip` 880 KB → 674 KB (−23%);
    // Linux `--gc-sections` 5.13 MB → 4.18 MB (−19%).
    //
    // Skipped for freestanding (the bare-metal user owns the link script and
    // may rely on sections `--gc-sections` would collect) and for shared-library
    // output (the host may call symbols this TU never references). `-fno-dead-strip`
    // opts out everywhere.
    if !no_libc && !shared && dead_strip {
        if std::env::consts::OS == "macos" {
            // ld64 collects at symbol/section granularity; per-function sections
            // add nothing (measured identical output).
            clang_args.push("-Wl,-dead_strip".to_string());
        } else {
            // GNU ld / lld / mingw: --gc-sections only sees whole sections, so
            // the compiler must emit one section per function/variable first.
            clang_args.push("-ffunction-sections".to_string());
            clang_args.push("-fdata-sections".to_string());
            clang_args.push("-Wl,--gc-sections".to_string());
        }
    }
    // Auto-link the precompiled Foundation library when one is findable —
    // same policy as libjeti: found → its real tables win the link; not
    // found → silent no-op (a TU that imports Foundation.jeti inlines the
    // implementations itself and needs no library). Skipped in freestanding
    // (the bare-metal user provides everything), shared mode (the host owns
    // the Foundation), and when any TU already inlines Foundation
    // implementations (`foundation_impl_inlined`, computed at the source
    // level in main): the archive's STRONG Foundation-only vtable would win
    // the weak merge and trip the __sig cross-TU check (multi_tu case 12).
    // Auto-link is for declaration-only clients (`Foundation.hje`) — the
    // ObjC model. An explicit `-l jetifoundation` suppresses the auto one
    // (no duplicate).
    //
    // Static archive only, deliberately: linking the shared Foundation is
    // prototyped but broken for KVC-driven code — see the KNOWN ISSUE note on
    // `foundation_shared_name` above. Do not switch this to the shared library
    // without fixing the metadata interposition first.
    if !no_libc && !shared && !foundation_impl_inlined && !libs.iter().any(|l| l == "jetifoundation") {
        if let Some(fdir) = find_libjetifoundation(lib_dirs) {
            if verbose { eprintln!("[jetic]   auto-linking Foundation from {}", fdir); }
            clang_args.push("-L".to_string());
            clang_args.push(fdir);
            clang_args.push("-ljetifoundation".to_string());
        }
    }
    // User link flags: -L dirs then -l libs, right before the implicit-libs
    // tail — linker resolution order matters (libs must follow the objects,
    // which are streamed via stdin at the end of clang_args).
    for d in lib_dirs {
        clang_args.push("-L".to_string());
        clang_args.push(d.clone());
    }
    for l in libs {
        clang_args.push(format!("-l{}", l));
    }
    // Backend diagnostics are NOT suppressed by default: `-w` used to hide
    // codegen-quality noise, but that also hid every C-standard violation
    // the front end doesn't catch yet. The generated C must be warning-clean
    // for a clean .jeti — fix codegen at the source instead of silencing cc.

    let mut child = match Command::new(program)
        .args(&cc_extra)
        .args(&clang_args)
        .stdin(Stdio::piped())        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("\x1b[1;31merror:\x1b[0m failed to execute clang: {}", e);
            std::process::exit(1);
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(c_code.as_bytes()) {
            eprintln!("\x1b[1;31merror:\x1b[0m failed to write to clang stdin: {}", e);
            std::process::exit(1);
        }
        if verbose { eprintln!("[jetic]   wrote {} bytes to clang", c_code.len()); }
    }
    if verbose { eprintln!("[jetic]   waiting for clang..."); }

    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("\x1b[1;31merror:\x1b[0m clang failed: {}", e);
            std::process::exit(1);
        }
    };

    if verbose { eprintln!("[jetic]   clang finished with status: {}", status); }

    if !status.success() {
        eprintln!("Compilation failed");
        std::process::exit(1);
    }
}

/// All jetic flags, grouped by dash count.
/// Returns (single_dash_flags, double_dash_flags).
fn jetic_flags() -> (Vec<(&'static str, &'static str)>, Vec<(&'static str, &'static str)>) {
    let single = vec![
        ("-Werror",       "promote warnings to errors"),
        ("-emit-bridge-header", "emit a C bridge header for calling Jeti from C"),
        ("-rewrite-jeti", "transpile to C only (no link)"),
        ("-fjeti-arc",    "enable ARC (default)"),
        ("-fno-jeti-arc", "disable ARC (MRC)"),
        ("-fno-checker",  "skip type checking"),
        ("-eh",           "exception backend: checked or sjlj (default sjlj; 'legacy' is an alias)"),
        ("-ffreestanding", "bare-metal/freestanding output"),
        ("-nostdinc",     "pass -nostdinc to the C compiler (headers come from your -I dirs)"),
        ("-no-comments",  "omit readability comments in generated C (default: on)"),
        ("-trace-refcount", "print a static reference-count trace (no codegen)"),
        ("-trace-max-iters", "loop iterations in the refcount trace (default 2)"),
        ("-trace-no-color", "disable colors in the refcount trace"),
        ("-backend",      "C compiler backend (clang, portable, gcc)"),
        ("-v",            "verbose transpilation"),
        ("-o",            "output path (binary or .c)"),
        ("-I",            "add include dir"),
        ("-L",            "add lib dir"),
        ("-l",            "link a library (e.g. -l gmp; -I/-L/-l also accept joined form)"),
        ("-asm",          "link a real assembly file (repeatable)"),
        ("-S",            "link a real assembly file (repeatable)"),
        ("-arch",         "target arch (e.g. -arch x86_64)"),
    ];
    let double = vec![
        ("--verbose", "show verbose transpilation info"),
        ("--version", "print version and exit"),
    ];
    (single, double)
}

/// Interactive flag picker: shows all matching flags and lets the user select one.
/// `prefix` is either "-" or "--". Returns the chosen flag, or None on cancel/error.
fn interactive_flag_picker(prefix: &str) -> Option<&'static str> {
    let (single, double) = jetic_flags();
    let (list, label) = if prefix == "--" {
        (double, "double-dash")
    } else {
        (single, "single-dash")
    };

    if list.is_empty() {
        eprintln!("No {} flags available.", label);
        return None;
    }

    println!("=== Jeti {} flags ===", label);
    for (i, (flag, desc)) in list.iter().enumerate() {
        println!("  {:>2}) {}  \x1b[2m{}\x1b[0m", i + 1, flag, desc);
    }
    println!("  {}  (cancel)", list.len() + 1);
    print!("Select (1-{}): ", list.len() + 1);
    let _ = std::io::stdout().flush();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        eprintln!("error reading input");
        return None;
    }
    let trimmed = line.trim();

    if trimmed.is_empty() || trimmed == "0" || trimmed == (list.len() + 1).to_string() {
        println!("Cancelled.");
        return None;
    }

    if let Ok(n) = trimmed.parse::<usize>() {
        if n >= 1 && n <= list.len() {
            let (flag, _) = list[n - 1];
            println!("Selected: {}", flag);
            return Some(flag);
        }
    }

    // Allow typing the flag name directly
    if let Some((flag, _)) = list.iter().find(|(f, _)| *f == trimmed) {
        println!("Selected: {}", flag);
        return Some(flag);
    }

    eprintln!("Invalid selection: {}", trimmed);
    None
}

fn norm_flag(s: &str) -> &str {
    if s.starts_with("--") {
        match s {
            "--rewrite-jeti" => "-rewrite-jeti",
            "--fjeti-arc" => "-fjeti-arc",
            "--fno-jeti-arc" => "-fno-jeti-arc",
            "--fno-checker" => "-fno-checker",
            "--eh" => "-eh",
            "--ffreestanding" => "-ffreestanding",
            "--nostdinc" => "-nostdinc",
            "--no-comments" => "-no-comments",
            "--trace-refcount" => "-trace-refcount",
            "--trace-max-iters" => "-trace-max-iters",
            "--trace-no-color" => "-trace-no-color",
            "--Werror" | "--werror" => "-Werror",
            "--emit-bridge-header" => "-emit-bridge-header",
            "--backend" => "-backend",
            "--arch" => "-arch",
            "--asm" => "-asm",
            "--verbose" => "-v",
            "--version" => "-V",
            _ => s,
        }
    } else {
        s
    }
}

/// Split `-flag=value` / `--flag=value` into a normalized flag and its inline
/// value. Without `=`, returns (normalized_flag, None) so callers fall back to
/// consuming the next argument. Currently applied to `-backend=<mode>`.
/// Optimization level accepted as a pure passthrough to the C compiler:
/// `-O`, `-O0`, `-O1`, `-O2`, `-O3`, `-Og`, `-Os`, `-Oz`, `-Ofast`.
///
/// jetic itself has no optimization level — it emits the same C either way — so
/// without the flag nothing is forwarded and the C compiler's own default
/// applies, exactly as before. Accepting the flag only stops these spellings
/// from being mistaken for input file names.
fn is_opt_flag(a: &str) -> bool {
    matches!(a, "-O" | "-O0" | "-O1" | "-O2" | "-O3" | "-Og" | "-Os" | "-Oz" | "-Ofast")
}

fn split_flag_value(arg: &str) -> (&str, Option<&str>) {
    match arg.find('=') {
        Some(eq) => {
            let flag = norm_flag(&arg[..eq]);
            (flag, arg.get(eq + 1..))
        }
        None => (norm_flag(arg), None),
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let version = env!("JETI_VERSION");

    // Bare "-" or "--" triggers an interactive flag picker.
    if args.len() > 1 && (args[1] == "-" || args[1] == "--") {
        if let Some(flag) = interactive_flag_picker(&args[1]) {
            // Re-execute with the chosen flag in place of the bare "-"/"--".
            let mut new_args: Vec<String> = Vec::with_capacity(args.len());
            new_args.push(args[0].clone());
            new_args.push(flag.to_string());
            new_args.extend_from_slice(&args[2..]);
            let status = std::process::Command::new(&new_args[0])
                .args(&new_args[1..])
                .status();
            match status {
                Ok(s) => std::process::exit(s.code().unwrap_or(1)),
                Err(e) => {
                    eprintln!("error re-executing jetic: {}", e);
                    std::process::exit(1);
                }
            }
        } else {
            std::process::exit(1);
        }
    }

    // Check for --version / -V first (before any other parsing)
    if args.len() > 1 && (args[1] == "--version" || args[1] == "-V") {
        println!("jetic version {}", version);
        return;
    }

    // -gen-completions <shell> / --gen-completions <shell>: emit a completion script.
    if let Some(idx) = args.iter().position(|a| a == "--gen-completions" || a == "-gen-completions") {
        if let Some(shell) = args.get(idx + 1) {
            gen_completions(shell);
        } else {
            eprintln!("error: -gen-completions requires a shell name (bash|zsh|fish|powershell|elvish)");
            std::process::exit(1);
        }
        return;
    }

    // Hidden --autocomplete mode (mirrors clang): prints matching flags for shell Tab completion.
    // The shell script calls `jetic --autocomplete=<whole-command-so-far>` or
    // `jetic --autocomplete "<whole-command-so-far>"`.
    if let Some(ac_arg) = args.iter().find(|a| a.starts_with("--autocomplete")) {
        // Strip the "--autocomplete[...]" prefix to get the raw command line.
        let raw = ac_arg.trim_start_matches("--autocomplete");
        let joined = match raw.strip_prefix('=') {
            Some(rest) => rest.to_string(),
            None => {
                // Could be `--autocomplete <args>`: collect following args.
                let idx = args.iter().position(|a| a == "--autocomplete");
                match idx {
                    Some(i) => args[i + 1..].join(" "),
                    None => String::new(),
                }
            }
        };
        let (single, double) = jetic_flags();
        let all = single.iter().chain(double.iter());
        // The word being completed is the last token — the shell joins words with
        // commas (like clang), so split on commas and whitespace.
        let cur = joined
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .last()
            .unwrap_or("");
        for (flag, desc) in all {
            if flag.starts_with(cur) && !cur.is_empty() {
                println!("{}\t{}", flag, desc);
            }
        }
        return;
    }

    if args.len() < 2 || args[1] == "-h" || args[1] == "--help" {
        println!("Usage: jetic [command] [options] <input.jeti> [more.jeti|file.o|lib.a ...]");
        println!();
        println!("Commands:");
        println!("  run                 Compile, run, then delete binary");
        println!();
        println!("Modes (default: compile to binary):");
        println!("  -rewrite-jeti       Transpile to C only (no link; single input)");
        println!();
        println!("Multi-TU (compile mode): extra positional .jeti files become additional");
        println!("translation units compiled and linked into the same binary; .o/.a files");
        println!("are linked as-is. Example: jetic main.jeti lib.jeti -I include -o app");
        println!("Transpilation Options:");
        println!("  -o <path>                            Output path (binary or .c)");
        println!("  -I <dir>                             Add include directory");
        println!("  -L <dir>                             Add lib directory");
        println!("  -l <lib>                             Link a library (e.g. -l gmp)");
        println!("                                       (-I/-L/-l also accept joined -I<path> form)");
        println!("  -asm <file.s>                        Link a real assembly file (repeatable)");
        println!("  -arch <target>                       Build for target arch (e.g. -arch x86_64)");
        println!("  -v, --verbose                        Show verbose transpilation info");
        println!("  -V, --version                        Print version and exit");
        println!();
        println!("Runtime Options:");
        println!("  -fjeti-arc                           Enable ARC (default)");
        println!("  -g                                   Emit debug symbols (-g -O0 to the C");
        println!("                                     compiler); with -line-directives, LLDB/");
        println!("                                     GDB show .jeti/.hje file and line numbers");
        println!("  -O0|-O1|-O2|-O3|-Os|-Oz            Forwarded verbatim to the C compiler.");
        println!("                                     Default: -O3. A -g build keeps its");
        println!("                                     -g -O0 preset; an explicit level here");
        println!("                                     overrides either default");
        println!("  -line-directives                     Emit #line <file> in generated C so");
        println!("                                     clang diagnostics point back at");
        println!("                                     the .jeti/.hje source (default: on)");
        println!("  -fno-line-directives                 Disable #line emission: clang");
        println!("                                     diagnostics then report <stdin>");
        println!("                                     instead of .jeti/.hje locations");
        println!("  -np-map                              Emit a versioned .jeti.map sidecar next");
        println!("                                     to the .jeti source, mapping generated");
        println!("                                     C lines back to .jeti/.hje sources");
        println!("  -fno-dead-strip                      Keep unreferenced code/data at link");
        println!("                                     time. Dead stripping is ON by default");
        println!("                                     (-Wl,-dead_strip on macOS, --gc-sections");
        println!("                                     elsewhere); freestanding and shared-library");
        println!("                                     links skip it either way");
        println!("  -fno-jeti-arc                        Disable ARC (MRC)");
        println!("  -ffreestanding                       Bare-metal/freestanding output");
        println!("                                     (no libc headers, no TLS, no bundled");
        println!("                                     runtime. The C compiler gets");
        println!("                                     -ffreestanding -fno-builtin ... (NOT");
        println!("                                     -nostdinc; see below).");
        println!("  -nostdinc                            Pass -nostdinc to the C compiler:");
        println!("                                     strip system include paths — all");
        println!("                                     headers must come from -I dirs.");
        println!("  -no-comments                         Omit the readability comments in the");
        println!("                                     generated C code (comments are on by");
        println!("                                     default).");
        println!("  -backend <mode>                      C compiler backend (clang, portable, gcc)");
        println!("                                     Default: clang");
        println!("                                     portable: only gcc+clang common attributes");
        println!("                                     gcc:   allow gcc-specific __attribute__");
        println!("  -eh <mode>                           Exception backend:");
        println!("                                     checked (default): flag + guard lowering,");
        println!("                                       unwind-safe ARC, no setjmp/longjmp —");
        println!("                                       works on bare metal");
        println!("                                     legacy (alias: sjlj): old setjmp/longjmp");
        println!("                                       backend, kept as a full rollback");
        println!();
        println!("Type Checking Options:");
        println!("  -fno-checker                         Skip type checking");
        println!();
        println!("Refcount Trace (debug aid):");
        println!("  -trace-refcount                      Print a static reference-count trace");
        println!("                                     of each retained object, in source order.");
        println!("  -trace-max-iters <N>                 Loop iterations in the trace (default 2)");
        println!("  -trace-no-color                      Disable colors in the trace");
        println!();
        println!("Additional help:");
        println!("  -h, --help                           Print this help and exit");
        std::process::exit(0);
    }

    let mut i = 1;
    let mut mode = "compile";
    let mut input = None;
    let mut output = None;
    let mut include_dirs = Vec::new();
    let mut lib_dirs = Vec::new();
    let mut libs = Vec::new(); // -l <name> → clang -l<name>
    let mut asm_files = Vec::new();
    // Multi-TU mode: positional .jeti inputs after the first. Each is transpiled
    // in its own jetic pass and linked into the same binary.
    let mut extra_inputs: Vec<String> = Vec::new();
    // Precompiled objects/archives passed as extra positionals: linked as-is.
    let mut extra_objects: Vec<String> = Vec::new();
    let mut frameworks = Vec::new();
    let mut no_arc = false;
    let mut no_checker = false;  // ARC mode by default
    let mut no_libc = false;     // bare-metal / freestanding mode
    let mut debug_symbols = false; // -g: emit DWARF via -g -O0 (plan 阶段 3)
    // -O<level>: a pure passthrough to the C compiler. jetic emits the same C
    // whatever the level, so leaving it unset keeps today's behaviour exactly
    // (the C compiler's own default applies).
    let mut opt_level: Option<String> = None;
    let mut nostdinc = false;    // strip system include paths (orthogonal flag)
    let mut no_comments = false; // readability comments in generated C (on by default)
    let mut jeti_map = false;      // -np-map: emit a .jeti.map sidecar source map
    let mut shared = false;      // dynamic library output (-shared/-dynamiclib)
    // Drop unreferenced functions/data at link time. ON by default: Jeti's
    // linking model pulls whole archive members (a declaration-only client
    // references every Foundation class it sees), so plenty of the linked
    // Foundation is never reached — measured on a Foundation hello-world,
    // `-Wl,-dead_strip` alone takes 880 KB → 674 KB (−23%). `-fno-dead-strip`
    // opts out (freestanding and shared-library links skip it regardless).
    let mut dead_strip = true;
    let mut werror = false;
    let mut verbose = false;
    let mut program_args = Vec::new();
    let mut arch: Option<String> = None;
    let mut backend: Option<String> = None;
    let mut trace_refcount = false;
    let mut trace_max_iters = 2;
    let mut trace_no_color = false;
    let mut bridge_header: Option<String> = None;
    // DEFAULT is the checked backend (pipeline::DEFAULT_EH_CHECKED). `-eh
    // legacy` / `-eh sjlj` select the old setjmp/longjmp backend for rollback.
    let mut eh_checked = DEFAULT_EH_CHECKED;
    let mut slots_manifest: Option<String> = None; // --slots <file> (stable cross-TU vtable layout)
    // --slots-seed <file> (internal): a library's read-only layout base,
    // discovered from `lib<name>.slots` and forwarded to every child TU.
    let mut slots_seed: Option<String> = None;
    // Internal multi-TU transport for true-generic specialization demands.
    let mut forced_generic_instantiations: Vec<String> = Vec::new();
    // `--instantiate-only`: this TU only contributes specializations out of a
    // template source shipped with a precompiled library, so it must not claim
    // R2 ownership of the classes it implements (see Pipeline::instantiate_only).
    let mut instantiate_only = false;
    // `--template-manifest <file>`: use this manifest instead of discovering
    // `lib<name>.templates` next to the archives the link pulls in.
    let mut template_manifest: Option<String> = None;
    // Internal multi-TU transport: where to write this TU's demand manifest
    // (`--emit-generic-demands`). Only the driver inside jetic sets it.
    let mut generic_demands_out: Option<String> = None;

    // Check for "run" subcommand: look for `run` that is not preceded by a flag
    // (i.e. not `-o run` or `-I run`)
    let mut run_pos = None;
    for (pos, arg) in args.iter().enumerate() {
        if arg == "run" && pos > 0 {
            if pos > 1 && args[pos - 1].starts_with('-') && !matches!(args[pos - 1].as_str(), "-v" | "--verbose") {
                continue;
            }
            run_pos = Some(pos);
            break;
        }
    }
    if let Some(pos) = run_pos {
        mode = "run";
        i = pos + 1;
        // Re-process flags before "run"
        for j in 1..pos {
            let (nj, inline_val) = split_flag_value(&args[j]);
            if nj == "-v" {
                verbose = true;
            } else if nj == "-fno-jeti-arc" {
                no_arc = true;
            } else if nj == "-fjeti-arc" {
                no_arc = false;
            } else if nj == "-fno-checker" {
                no_checker = true;
            } else if nj == "-ffreestanding" {
                no_libc = true;
            } else if nj == "-g" {
                debug_symbols = true;
            } else if is_opt_flag(nj) {
                opt_level = Some(nj.to_string());
            } else if nj == "-nostdinc" {
                nostdinc = true;
            } else if nj == "-no-comments" {
                no_comments = true;
            } else if nj == "-fdead-strip" {
                dead_strip = true;
            } else if nj == "-fno-dead-strip" {
                dead_strip = false;
            } else if nj == "-Werror" {
                werror = true;
            } else if nj == "-arch" && j + 1 < pos {
                arch = Some(args[j + 1].clone());
            } else if nj == "-backend" && j + 1 < pos {
                backend = Some(args[j + 1].clone());
            } else if nj == "-backend" && inline_val.is_some() {
                backend = inline_val.map(|s| s.to_string());
            } else if nj == "-trace-refcount" {
                trace_refcount = true;
            } else if nj == "-trace-max-iters" && j + 1 < pos {
                trace_max_iters = args[j + 1].parse().unwrap_or(2);
            } else if nj == "-trace-no-color" {
                trace_no_color = true;
            } else if nj == "-emit-bridge-header" && j + 1 < pos {
                bridge_header = Some(args[j + 1].clone());
            } else if nj == "-eh" {
                let v = inline_val.map(|s| s.to_string())
                    .or_else(|| if j + 1 < pos { Some(args[j + 1].clone()) } else { None })
                    .unwrap_or_default();
                match v.as_str() {
                    "checked" => eh_checked = true,
                    // 'legacy' is the sjlj backend's official alias: the escape
                    // hatch for the checked-by-default flip. Explicitly sets the
                    // flag to false (the default is now true).
                    "sjlj" | "legacy" | "" => eh_checked = false,
                    other => {
                        eprintln!("error: -eh expects 'checked', 'sjlj' or 'legacy' (got '{}')", other);
                        std::process::exit(1);
                    }
                }
            } else if nj == "--slots" {
                slots_manifest = inline_val.map(|s| s.to_string())
                    .or_else(|| if j + 1 < pos { Some(args[j + 1].clone()) } else { None });
                if slots_manifest.is_none() {
                    eprintln!("error: --slots expects a manifest file path");
                    std::process::exit(1);
                }
            } else if nj == "--slots-seed" {
                slots_seed = inline_val.map(|s| s.to_string())
                    .or_else(|| if j + 1 < pos { Some(args[j + 1].clone()) } else { None });
                if slots_seed.is_none() {
                    eprintln!("error: --slots-seed expects a manifest file path");
                    std::process::exit(1);
                }
            } else if nj == "--generic-instantiation" {
                if let Some(v) = inline_val.map(|s| s.to_string()).or_else(|| if j + 1 < pos { Some(args[j + 1].clone()) } else { None }) {
                    forced_generic_instantiations.push(v);
                } else {
                    eprintln!("error: --generic-instantiation expects Class<Args>");
                    std::process::exit(1);
                }
            } else if nj == "--emit-generic-demands" {
                generic_demands_out = inline_val.map(|s| s.to_string())
                    .or_else(|| if j + 1 < pos { Some(args[j + 1].clone()) } else { None });
                if generic_demands_out.is_none() {
                    eprintln!("error: --emit-generic-demands expects a file path");
                    std::process::exit(1);
                }
            }
        }
    }

    while i < args.len() {
        let (normalized, inline_val) = split_flag_value(&args[i]);
        if normalized == "-rewrite-jeti" {
            mode = "rewrite";
            i += 1;
        } else if normalized == "-fno-jeti-arc" {
            no_arc = true;
            i += 1;
        } else if normalized == "-fjeti-arc" {
            no_arc = false;
            i += 1;
        } else if normalized == "-fno-checker" {
            no_checker = true;
            i += 1;
        } else if normalized == "-ffreestanding" {
            no_libc = true;
            i += 1;
        } else if normalized == "-g" {
            debug_symbols = true;
            i += 1;
        } else if is_opt_flag(normalized) {
            opt_level = Some(normalized.to_string());
            i += 1;
        } else if normalized == "-nostdinc" {
            nostdinc = true;
            i += 1;
        } else if normalized == "-no-comments" {
            no_comments = true;
            i += 1;
        } else if normalized == "-fdead-strip" {
            dead_strip = true;
            i += 1;
        } else if normalized == "-fno-dead-strip" {
            dead_strip = false;
            i += 1;
        } else if normalized == "-shared" {
            shared = true;
            i += 1;
        } else if normalized == "-Werror" {
            werror = true;
            i += 1;
        } else if normalized == "-trace-refcount" {
            trace_refcount = true;
            i += 1;
        } else if normalized == "-trace-max-iters" && i + 1 < args.len() {
            trace_max_iters = args[i + 1].parse().unwrap_or(2);
            i += 2;
        } else if normalized == "-trace-no-color" {
            trace_no_color = true;
            i += 1;
        } else if normalized == "-eh" {
            let (val, adv) = if let Some(iv) = inline_val {
                (Some(iv.to_string()), 1)
            } else if i + 1 < args.len() {
                (Some(args[i + 1].clone()), 2)
            } else {
                (None, 1)
            };
            match val.as_deref() {
                Some("checked") => { eh_checked = true; i += adv; }
                // 'legacy' = sjlj alias — the rollback for the checked default.
                Some("sjlj") | Some("legacy") => { eh_checked = false; i += adv; }
                other => {
                    eprintln!("error: -eh expects 'checked', 'sjlj' or 'legacy' (got '{}')",
                        other.unwrap_or("(missing)"));
                    std::process::exit(1);
                }
            }
        } else if normalized == "--slots" {
            let (val, adv) = if let Some(iv) = inline_val {
                (Some(iv.to_string()), 1)
            } else if i + 1 < args.len() {
                (Some(args[i + 1].clone()), 2)
            } else {
                (None, 1)
            };
            match val {
                Some(v) => { slots_manifest = Some(v); i += adv; }
                None => {
                    eprintln!("error: --slots expects a manifest file path");
                    std::process::exit(1);
                }
            }
        } else if normalized == "--slots-seed" {
            let (val, adv) = if let Some(iv) = inline_val {
                (Some(iv.to_string()), 1)
            } else if i + 1 < args.len() {
                (Some(args[i + 1].clone()), 2)
            } else {
                (None, 1)
            };
            match val {
                Some(v) => { slots_seed = Some(v); i += adv; }
                None => {
                    eprintln!("error: --slots-seed expects a manifest file path");
                    std::process::exit(1);
                }
            }
        } else if normalized == "--generic-instantiation" {
            let (val, adv) = if let Some(iv) = inline_val {
                (Some(iv.to_string()), 1)
            } else if i + 1 < args.len() {
                (Some(args[i + 1].clone()), 2)
            } else { (None, 1) };
            match val {
                Some(v) => { forced_generic_instantiations.push(v); i += adv; }
                None => {
                    eprintln!("error: --generic-instantiation expects Class<Args>");
                    std::process::exit(1);
                }
            }
        } else if normalized == "--instantiate-only" {
            instantiate_only = true;
            i += 1;
        } else if normalized == "--template-manifest" {
            let (val, adv) = if let Some(iv) = inline_val {
                (Some(iv.to_string()), 1)
            } else if i + 1 < args.len() {
                (Some(args[i + 1].clone()), 2)
            } else { (None, 1) };
            match val {
                Some(v) => { template_manifest = Some(v); i += adv; }
                None => {
                    eprintln!("error: --template-manifest expects a file path");
                    std::process::exit(1);
                }
            }
        } else if normalized == "-arch" && i + 1 < args.len() {
            arch = Some(args[i + 1].clone());
            i += 2;
        } else if normalized == "-backend" {
            let (val, adv) = if let Some(iv) = inline_val {
                (Some(iv.to_string()), 1)
            } else if i + 1 < args.len() {
                (Some(args[i + 1].clone()), 2)
            } else {
                (None, 1)
            };
            match val {
                Some(b) => { backend = Some(b); i += adv; }
                None => {
                    eprintln!("error: -backend requires a mode (clang, portable, or gcc)");
                    std::process::exit(1);
                }
            }
        } else if normalized == "-v" {
            verbose = true;
            i += 1;
        } else if normalized == "-o" && i + 1 < args.len() {
            output = Some(args[i + 1].clone());
            i += 2;
        } else if normalized == "-I" && i + 1 < args.len() {
            include_dirs.push(args[i + 1].clone());
            i += 2;
        } else if normalized.starts_with("-I") && normalized.len() > 2 {
            // Joined form `-I/path` (clang/GCC convention)
            include_dirs.push(normalized[2..].to_string());
            i += 1;
        } else if normalized == "-l" && i + 1 < args.len() {
            libs.push(args[i + 1].clone());
            i += 2;
        } else if normalized == "-line-directives" || normalized == "--line-directives" {
            // Must be matched BEFORE the joined `-l…` branch below — the flag
            // starts with `-l`, and `-lgmp`-style parsing would turn it into
            // `link library "ine-directives"`. The flag value itself is read
            // after this loop (pipeline.line_directives).
            i += 1;
        } else if normalized == "-fno-line-directives" || normalized == "--fno-line-directives" {
            i += 1;
        } else if normalized == "-np-map" || normalized == "--np-map" {
            // Plan 阶段 4: emit the versioned sidecar source map.
            jeti_map = true;
            i += 1;
        } else if normalized.starts_with("-l") && normalized.len() > 2
            && normalized[2..].chars().next().map_or(false, |c| c.is_ascii_alphanumeric())
        {
            // Joined form `-lgmp`. Must come AFTER `-lgmp`-shaped unknown-flag
            // handling would reject it — accepted here so link libraries follow
            // the usual clang spelling. (Plain identifiers only: `-last`-style
            // ambiguity is the user's to resolve with the spaced form.)
            libs.push(normalized[2..].to_string());
            i += 1;
        } else if (normalized == "-asm" || normalized == "-S") && i + 1 < args.len() {
            asm_files.push(args[i + 1].clone());
            i += 2;
        } else if normalized == "-framework" && i + 1 < args.len() {
            frameworks.push(args[i + 1].clone());
            i += 2;
        } else if normalized == "-L" && i + 1 < args.len() {
            lib_dirs.push(args[i + 1].clone());
            i += 2;
        } else if normalized.starts_with("-L") && normalized.len() > 2 {
            // Joined form `-L/path` (clang/GCC convention)
            lib_dirs.push(normalized[2..].to_string());
            i += 1;
        } else if normalized == "-emit-bridge-header" && i + 1 < args.len() {
            bridge_header = Some(args[i + 1].clone());
            i += 2;
        } else if normalized == "--emit-generic-demands" && i + 1 < args.len() {
            // Internal multi-TU transport: write the specialization manifest
            // this TU demands (direct + transitively derived) so the driver can
            // feed it back to the sibling TUs.
            generic_demands_out = Some(args[i + 1].clone());
            i += 2;
        } else if input.is_none() {
            input = Some(args[i].clone());
            i += 1;
        } else if mode == "compile"
            && (args[i].ends_with(".jeti") || args[i].ends_with(".o") || args[i].ends_with(".a"))
        {
            // Additional input (multi-input mode). .jeti → an extra translation
            // unit (transpiled in its own jetic pass); .o/.a → linked as-is.
            // Compile mode only: in run mode positionals are program arguments,
            // and -rewrite-jeti emits one .c per invocation.
            if args[i].ends_with(".jeti") {
                extra_inputs.push(args[i].clone());
            } else {
                extra_objects.push(args[i].clone());
            }
            i += 1;
        } else if mode == "rewrite" && args[i].ends_with(".jeti") {
            eprintln!("error: multiple inputs are supported in compile mode only — run one -rewrite-jeti per file");
            std::process::exit(1);
        } else if mode == "run" {
            program_args.push(args[i].clone());
            i += 1;
        } else {
            eprintln!("Unknown argument: {}", args[i]);
            std::process::exit(1);
        }
    }

    let input_path = input.unwrap_or_else(|| {
        eprintln!("No input file specified");
        std::process::exit(1);
    });

    if trace_refcount && !extra_inputs.is_empty() {
        eprintln!("error: -trace-refcount traces one translation unit; drop the extra inputs");
        std::process::exit(1);
    }

    let source = match fs::read_to_string(&input_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("\x1b[1;31merror:\x1b[0m cannot read {}: {}", input_path, e);
            std::process::exit(1);
        }
    };

    // In a multi-TU build, specialization demand is a property of the whole
    // link unit. Forward every concrete `Box<T>` spelling seen in any source
    // to every transpilation pass; a TU emits it only when it also contains the
    // generic implementation body.
    if !extra_inputs.is_empty() {
        let mut all = forced_generic_instantiations;
        all.extend(scan_generic_instantiations(&source));
        for tu in &extra_inputs {
            if let Ok(s) = fs::read_to_string(tu) { all.extend(scan_generic_instantiations(&s)); }
        }
        all.sort();
        all.dedup();
        forced_generic_instantiations = all;
    }

    // ── Multi-TU specialization closure (Cfront-style manifest iteration) ──
    //
    // Direct demand (`ChainBox<NPString *>` spelled in a source file) is found
    // by the text scan above. Transitive demand is not: cloning
    // `ChainBox<NPString *>` inside its owner TU discovers `ChainArray<T>` in
    // the body and demands `ChainArray<NPString *>`, while the sibling TU that
    // owns `ChainArray` spells it param-keyed — invisible to any text scan.
    //
    // So each TU publishes what it demanded (`--emit-generic-demands`) and the
    // driver feeds the union back as `--generic-instantiation` until the set
    // stops growing. Only spellings travel between processes; the type
    // substitution stays inside the compiler, so every specialization still has
    // exactly one authoritative definition (R2).
    //
    // The main TU takes part in the loop too — a closure demand can land on a
    // template IT owns — so it runs as a child as well; its own C output is
    // still produced by the in-process transpile below (the child pass only
    // contributes its manifest).
    // A precompiled library may ship its pinned slot assignment beside the
    // archive (`lib<name>.slots`). Freeze this build onto that prefix unless the
    // user chose a manifest explicitly — otherwise the client's own imports can
    // shift a library class's slot indices and R3 aborts at startup. Discovered
    // before any child TU is spawned so siblings and template instantiations
    // inherit the identical base.
    if slots_seed.is_none() && slots_manifest.is_none() {
        slots_seed = find_slots_seed(&lib_dirs, &libs);
    }
    let mut extra_c_files: Vec<String> = Vec::new();
    if !extra_inputs.is_empty() {
        let exe = std::env::current_exe().unwrap_or_else(|e| {
            eprintln!("error: cannot locate jetic for multi-TU transpile: {}", e);
            std::process::exit(1);
        });
        let exe = exe.to_string_lossy().to_string();
        let pass = TuPass {
            exe: &exe,
            include_dirs: &include_dirs,
            no_arc,
            no_checker,
            werror,
            eh_checked,
            slots_manifest: slots_manifest.as_ref(),
            slots_seed: slots_seed.as_ref(),
            backend: backend.as_ref(),
            arch: arch.as_ref(),
            no_libc,
            nostdinc,
            no_comments,
            verbose,
        };
        let tmp_base = |tag: &str| {
            let mut p = std::env::temp_dir();
            p.push(format!("{}-{}-jetic", tag, std::process::id()));
            p.to_string_lossy().to_string()
        };
        for (n, tu) in extra_inputs.iter().enumerate() {
            let stem = Path::new(tu).file_stem().and_then(|s| s.to_str()).unwrap_or("tu");
            extra_c_files.push(format!("{}-{}-{}.c", tmp_base(stem), n, stem));
        }
        // Nothing to close without a single concrete demand: keep the plain
        // one-pass transpile (no extra child runs at all).
        let closure = !forced_generic_instantiations.is_empty();
        let mut forced = forced_generic_instantiations.clone();
        // Fixed point: each round transcribes every TU with the current set and
        // unions what they demand. Monotone growth over a finite spelling space
        // guarantees termination; the cap only guards a pathological input.
        let mut rounds = if closure { 8 } else { 1 };
        while rounds > 0 {
            rounds -= 1;
            let mut next: Vec<String> = forced.clone();
            if closure {
                next.extend(pass.run(
                    &input_path,
                    &format!("{}-main.c", tmp_base("main")),
                    &format!("{}-main.demands", tmp_base("main")),
                    &forced,
                    true,
                ));
            }
            for (n, tu) in extra_inputs.iter().enumerate() {
                let demands = format!("{}-{}.demands", tmp_base("tu"), n);
                // stderr stays connected: an extra TU's warnings are the user's
                // and were visible before the closure loop existed.
                next.extend(pass.run(tu, &extra_c_files[n], &demands, &forced, false));
            }
            next.sort();
            next.dedup();
            if next == forced {
                break;
            }
            forced = next;
        }
        forced_generic_instantiations = forced;
    }

    let self_dir = resolve_bundle_root();
    let include_root = self_dir.join("include");
    let foundation_include = self_dir.join("include").join("Foundation");

    let mut pipeline = Pipeline::new();
    pipeline.search_dirs.clear();
    pipeline.search_dirs.push(include_root.to_string_lossy().to_string());
    pipeline.search_dirs.push(foundation_include.to_string_lossy().to_string());
    pipeline.search_dirs.push(".".to_string());
    pipeline.search_dirs.extend(include_dirs.clone());
    pipeline.no_arc = no_arc;
    pipeline.no_checker = no_checker;
    pipeline.no_libc = no_libc;
    pipeline.nostdinc = nostdinc;
    pipeline.no_comments = no_comments;
    pipeline.werror = werror;
    pipeline.bridge_header = bridge_header;
    pipeline.verbose = verbose;
    pipeline.trace_refcount = trace_refcount;
    pipeline.trace_max_iters = trace_max_iters;
    pipeline.trace_color = !trace_no_color;
    pipeline.eh_checked = eh_checked;
    pipeline.slots_manifest = slots_manifest.clone();
    pipeline.slots_seed = slots_seed.clone();
    pipeline.forced_generic_instantiations = forced_generic_instantiations.clone();
    pipeline.instantiate_only = instantiate_only;
    if let Some(ref b) = backend {
        match attrs::Backend::parse(b) {
            Some(be) => pipeline.backend = be,
            None => {
                eprintln!("error: unknown backend '{}' (try clang, portable, or gcc)", b);
                std::process::exit(1);
            }
        }
    }

    // C compiler for the link step: default `clang`; `-backend gcc` → `gcc`;
    // `$JETI_CC` overrides. (Windows default is also clang.)
    let cc = select_c_compiler(pipeline.backend);
    // The C type-name probe (see `ctype_probe`) runs the same compiler's
    // preprocessor, so it needs the same program and target arch.
    pipeline.c_cc = cc.clone();
    pipeline.c_arch = arch.clone();
    // Escape hatch, same spirit as `$JETI_CC`: skip the probe entirely (no
    // compiler is spawned, no header is read). The parser then falls back to
    // the builtin type list and its shape heuristics.
    pipeline.no_ctype_probe = std::env::var_os("JETI_NO_CTYPE_PROBE").is_some();
    // `-line-directives` (plan 阶段 2, DEFAULT ON): emit `#line` in the
    // generated C so clang diagnostics point back at the .jeti/.hje.
    // `-fno-line-directives` turns it off.
    pipeline.line_directives = !args.iter().any(|a| a == "-fno-line-directives" || a == "--fno-line-directives");
    // `-np-map` (plan 阶段 4): the pipeline extracts the sidecar from the final
    // generated C; the CLI decides where (and whether) it lands on disk.
    pipeline.jeti_map = jeti_map;

    let c_code = match pipeline.transpile(&source, &input_path) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("\x1b[1;31merror:\x1b[0m {}", e);
            std::process::exit(1);
        }
    };

    // Internal multi-TU transport: publish what this TU demanded (direct +
    // transitively derived) so the driver can close the specialization chain
    // across sibling TUs. Not part of the public CLI surface.
    if let Some(ref path) = generic_demands_out {
        let mut manifest = pipeline.last_generic_demands.join("\n");
        if !manifest.is_empty() { manifest.push('\n'); }
        if let Err(e) = fs::write(path, manifest) {
            eprintln!("error: cannot write generic demands to {}: {}", path, e);
            std::process::exit(1);
        }
    }

    if trace_refcount {
        println!("{}", c_code);
        return;
    }

    // Multi-TU extra TUs were transpiled above by the closure driver, which
    // also owns their output paths (`extra_c_files`).

    // Does any TU in this binary inline Foundation implementations? Checked
    // at the SOURCE level (see tu_imports_foundation_impl) across the main
    // input and every extra TU — a mixed binary must not auto-link either.
    let foundation_impl_inlined = tu_imports_foundation_impl(&input_path)
        || extra_inputs.iter().any(|t| tu_imports_foundation_impl(t));

    // ── Specializations a precompiled archive cannot satisfy by itself ──
    //
    // An archive ships metadata only for the specializations its own TUs were
    // built with. A client that spells a NEW one gets codegen's empty weak stub,
    // which links fine and jumps to NULL on the first dispatch. If the library
    // also shipped its template sources (`lib<name>.templates`), close the gap
    // by compiling just those demands — the same Cfront-style manifest closure
    // the multi-TU path runs between source TUs, one level down.
    if mode != "rewrite" && !instantiate_only {
        let manifests = find_template_manifests(template_manifest.as_ref(), &lib_dirs, &libs);
        if !manifests.is_empty() {
            let mut demands = forced_generic_instantiations.clone();
            demands.extend(pipeline.last_generic_demands.iter().cloned());
            demands.sort();
            demands.dedup();
            if !demands.is_empty() {
                let exe = std::env::current_exe().unwrap_or_else(|e| {
                    eprintln!("error: cannot locate jetic for the template pass: {}", e);
                    std::process::exit(1);
                });
                let exe = exe.to_string_lossy().to_string();
                let pass = TuPass {
                    exe: &exe,
                    include_dirs: &include_dirs,
                    no_arc,
                    no_checker,
                    werror,
                    eh_checked,
                    slots_manifest: slots_manifest.as_ref(),
                    slots_seed: slots_seed.as_ref(),
                    backend: backend.as_ref(),
                    arch: arch.as_ref(),
                    no_libc,
                    nostdinc,
                    no_comments,
                    verbose,
                };
                extra_c_files.extend(instantiate_shipped_templates(&pass, &manifests, &demands));
            }
        }
    }

    // Plan 阶段 4: write the versioned sidecar source map (.jeti.map) next to
    // the .jeti source. The generated C's location differs per mode:
    // -rewrite-jeti writes a real .c; compile/run pipe the C to the compiler
    // through stdin (recorded as "<stdin>" — the content hash still ties the
    // map to the exact C bytes that were built).
    if jeti_map {
        if let Some(mut map) = pipeline.last_source_map.take() {
            map.generated.path = if mode == "rewrite" {
                output.clone().unwrap_or_else(|| {
                    if input_path.ends_with(".jeti") {
                        Path::new(&input_path).with_extension("c").display().to_string()
                    } else {
                        input_path.clone() + ".c"
                    }
                })
            } else {
                "<stdin>".to_string()
            };
            let sidecar_path = format!("{}.map", input_path);
            if let Some(parent) = Path::new(&sidecar_path).parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            match fs::write(&sidecar_path, map.to_json()) {
                Ok(_) => {
                    if verbose {
                        eprintln!("[jetic] wrote source map {}", sidecar_path);
                    }
                }
                Err(e) => {
                    eprintln!("\x1b[1;31merror:\x1b[0m cannot write {}: {}", sidecar_path, e);
                    std::process::exit(1);
                }
            }
        }
    }

    match mode {
        "rewrite" => {
            // -rewrite-jeti: output C code to file
            let output_path = output.unwrap_or_else(|| {
                if input_path.ends_with(".jeti") {
                    Path::new(&input_path).with_extension("c").display().to_string()
                } else {
                    input_path.clone() + ".c"
                }
            });
            if let Some(parent) = Path::new(&output_path).parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            match fs::write(&output_path, &c_code) {
                Ok(_) => {}
                Err(e) => {
                    eprintln!("\x1b[1;31merror:\x1b[0m cannot write {}: {}", output_path, e);
                    std::process::exit(1);
                }
            }
        }
        "run" => {
            // run mode: compile, run, then kill + delete binary
            let input_stem = Path::new(&input_path).file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("a.out");
            let bin_path = output.unwrap_or_else(|| {
                let mut p = std::env::temp_dir();
                p.push(format!("{}{}", input_stem, std::env::consts::EXE_SUFFIX));
                p.to_string_lossy().to_string()
            });

            compile_to_binary(&cc, &c_code, &bin_path, &include_dirs, &lib_dirs, &libs, &asm_files, &extra_c_files, &extra_objects, &frameworks, arch.as_deref(), verbose, no_libc, nostdinc, false, foundation_impl_inlined, debug_symbols, opt_level.as_deref(), dead_strip, eh_checked);

            for t in &extra_c_files {
                let _ = fs::remove_file(t);
            }

            let run_status = Command::new(&bin_path)
                .args(&program_args)
                .status()
                .expect("failed to execute binary");

            let _ = fs::remove_file(&bin_path);

            // A program killed by a signal (SIGSEGV/SIGABRT/…) has no exit code:
            // `code()` is `None`, and the old `unwrap_or(1)` turned that into a
            // bare `1` with no output whatsoever — a segfault was
            // indistinguishable from a deliberate `return 1`, and a probe that
            // died looked like it had merely "exited quietly". Name the signal,
            // and report the shell's 128+N status so a caller can tell a crash
            // from a clean non-zero exit.
            match run_status.code() {
                Some(code) => std::process::exit(code),
                None => {
                    let sig = exit_signal(&run_status);
                    match sig {
                        Some(n) => eprintln!(
                            "\x1b[1;31merror:\x1b[0m program terminated by signal {} ({})",
                            n,
                            signal_name(n)
                        ),
                        None => eprintln!(
                            "\x1b[1;31merror:\x1b[0m program terminated abnormally"
                        ),
                    }
                    std::process::exit(sig.map_or(1, |n| 128 + n));
                }
            }
        }
        _ => {
            // compile mode: compile to binary, keep it
            let bin_path = output.unwrap_or_else(|| format!("a.out{}", std::env::consts::EXE_SUFFIX));

            if bin_path.ends_with(".c") {
                eprintln!("error: use -rewrite-jeti to output C code");
                std::process::exit(1);
            }
            let shared = shared
                || bin_path.ends_with(".dylib")
                || bin_path.ends_with(".so")
                || bin_path.ends_with(".dll");

            compile_to_binary(&cc, &c_code, &bin_path, &include_dirs, &lib_dirs, &libs, &asm_files, &extra_c_files, &extra_objects, &frameworks, arch.as_deref(), verbose, no_libc, nostdinc, shared, foundation_impl_inlined, debug_symbols, opt_level.as_deref(), dead_strip, eh_checked);

            for t in &extra_c_files {
                let _ = fs::remove_file(t);
            }
        }
    }
}
