// Probe: does the parser actually SET `nulls` on method parameter / return
// types? The end-to-end negative probe compiled and ran with no diagnostic, so
// the annotation is being lost somewhere between parser and checker. Assert at
// the earliest point to bisect the pipeline.
use jeti_cst::*;
use jeti_parser::Parser;

fn methods_of(src: &str) -> Vec<(Option<Nullability>, Vec<(String, Nullability)>)> {
    let mut p = Parser::new(src);
    let unit = p.parse_translation_unit().expect("unit");
    let mut out = Vec::new();
    for d in &unit.decls {
        if let CstDeclData::Class { methods, .. } = &d.data {
            for m in methods {
                let CstDeclData::Method { return_type, params, .. } = &m.data else { continue };
                let mut ps = Vec::new();
                let mut cur = params.as_ref().map(|b| &**b);
                while let Some(pp) = cur {
                    ps.push((
                        pp.name.clone().unwrap_or_default(),
                        pp.par_type.as_ref().map(|t| t.nulls).unwrap_or(Nullability::Unspecified),
                    ));
                    cur = pp.next.as_ref().map(|n| &**n);
                }
                out.push((return_type.as_ref().map(|t| t.nulls), ps));
            }
        }
    }
    out
}

#[test]
fn method_param_nonnull_is_recorded() {
    let got = methods_of("@interface S : NPObject\n- (void)need:(nonnull NPString *)s;\n@end\n");
    println!("nonnull param -> {:?}", got);
    assert_eq!(got.len(), 1, "expected 1 method, got {:?}", got);
    assert_eq!(got[0].1[0].1, Nullability::Nonnull, "nonnull NOT recorded: {:?}", got);
}

#[test]
fn method_param_nullable_is_recorded() {
    let got = methods_of("@interface S : NPObject\n- (void)maybe:(nullable NPString *)s;\n@end\n");
    println!("nullable param -> {:?}", got);
    assert_eq!(got[0].1[0].1, Nullability::Nullable, "nullable NOT recorded: {:?}", got);
}

#[test]
fn method_return_nullable_is_recorded() {
    let got = methods_of("@interface S : NPObject\n- (nullable NPString *)f;\n@end\n");
    println!("nullable return -> {:?}", got);
    assert_eq!(got[0].0, Some(Nullability::Nullable), "nullable return NOT recorded");
}

#[test]
fn underscore_form_is_recorded() {
    let got = methods_of("@interface S : NPObject\n- (void)need:(_Nonnull NPString *)s;\n@end\n");
    println!("_Nonnull param -> {:?}", got);
    assert_eq!(got[0].1[0].1, Nullability::Nonnull, "_Nonnull NOT recorded: {:?}", got);
}

#[test]
fn contextual_keyword_still_a_variable_name() {
    // C-superset guard: `nullable` / `nonnull` must NOT be hard keywords.
    let mut p = Parser::new("int nullable = 5;\nint nonnull = 6;\n");
    let unit = p.parse_translation_unit();
    assert!(!p.has_error(), "contextual keyword broke C code: {}", p.last_error());
    assert_eq!(unit.unwrap().decls.len(), 2);
}

#[test]
fn region_makes_unannotated_pointer_nonnull() {
    let got = methods_of(
        "NP_ASSUME_NONNULL_BEGIN\n\
         @interface R : NPObject\n\
         - (void)take:(NPString *)s;\n\
         @end\n\
         NP_ASSUME_NONNULL_END\n",
    );
    println!("region param -> {:?}", got);
    assert_eq!(got.len(), 1, "expected 1 method, got {:?}", got);
    assert_eq!(
        got[0].1[0].1,
        Nullability::Nonnull,
        "region default NOT applied: {:?}", got
    );
}

#[test]
fn region_does_not_touch_non_pointers() {
    // `int` is not a pointer: the region must leave it alone.
    let got = methods_of(
        "NP_ASSUME_NONNULL_BEGIN\n\
         @interface R : NPObject\n\
         - (void)f:(int)n;\n\
         @end\n\
         NP_ASSUME_NONNULL_END\n",
    );
    println!("region non-pointer -> {:?}", got);
    assert_eq!(got[0].1[0].1, Nullability::Unspecified, "non-pointer annotated: {:?}", got);
}

#[test]
fn explicit_nullable_beats_region_default() {
    let got = methods_of(
        "NP_ASSUME_NONNULL_BEGIN\n\
         @interface R : NPObject\n\
         - (void)opt:(nullable NPString *)s;\n\
         @end\n\
         NP_ASSUME_NONNULL_END\n",
    );
    println!("region opt-out -> {:?}", got);
    assert_eq!(got[0].1[0].1, Nullability::Nullable, "opt-out lost: {:?}", got);
}

#[test]
fn region_ends_after_end_marker() {
    let got = methods_of(
        "NP_ASSUME_NONNULL_BEGIN\n\
         @interface A : NPObject\n\
         - (void)in:(NPString *)s;\n\
         @end\n\
         NP_ASSUME_NONNULL_END\n\
         @interface B : NPObject\n\
         - (void)out:(NPString *)s;\n\
         @end\n",
    );
    println!("before/after region -> {:?}", got);
    assert_eq!(got[0].1[0].1, Nullability::Nonnull, "inside region: {:?}", got);
    assert_eq!(
        got[1].1[0].1,
        Nullability::Unspecified,
        "region leaked past _END: {:?}", got
    );
}

#[test]
fn unclosed_region_is_an_error() {
    let mut p = Parser::new("NP_ASSUME_NONNULL_BEGIN\n@interface R : NPObject\n@end\n");
    p.parse_translation_unit();
    assert!(p.has_error(), "unclosed region must be reported");
    assert!(
        p.last_error().contains("without a matching NP_ASSUME_NONNULL_END"),
        "wrong message: {}",
        p.last_error()
    );
}

#[test]
fn function_return_annotation_is_recorded() {
    // End-to-end runs prove nothing here: codegen never reads `nulls`, so a
    // dropped annotation still compiles and runs. Assert on the CST directly.
    fn ret_nulls(src: &str) -> Vec<Nullability> {
        let mut p = Parser::new(src);
        let unit = p.parse_translation_unit().expect("unit");
        let mut out = Vec::new();
        for d in &unit.decls {
            if let CstDeclData::Function { return_type, .. } = &d.data {
                out.push(
                    return_type
                        .as_ref()
                        .map(|t| t.nulls)
                        .unwrap_or(Nullability::Unspecified),
                );
            }
        }
        out
    }

    // Both spellings, prefix and postfix.
    let got = ret_nulls("nullable NPString *pref(void) { return 0; }\n");
    println!("fn return prefix -> {:?}", got);
    assert_eq!(got[0], Nullability::Nullable, "prefix not recorded: {:?}", got);

    let got = ret_nulls("NPString * _Nullable post(void) { return 0; }\n");
    println!("fn return postfix -> {:?}", got);
    assert_eq!(got[0], Nullability::Nullable, "postfix not recorded: {:?}", got);

    // Unannotated stays unspecified — the no-false-positive guard.
    let got = ret_nulls("NPString *plain(void) { return 0; }\n");
    println!("fn return plain -> {:?}", got);
    assert_eq!(got[0], Nullability::Unspecified, "plain annotated: {:?}", got);
}

#[test]
fn block_param_and_return_annotations_are_recorded() {
    // Bisect with CST assertions: the end-to-end probe cannot tell "annotation
    // dropped at parse" from "checker never looks at block params".
    fn typedef_block(src: &str) -> (Nullability, Vec<Nullability>) {
        let mut p = Parser::new(src);
        let unit = p.parse_translation_unit().expect("unit");
        for d in &unit.decls {
            if let CstDeclData::Typedef { alias_type: Some(at), .. } = &d.data {
                let ret = at.nulls;
                let mut ps = Vec::new();
                let mut cur = at.block_params.as_deref();
                while let Some(t) = cur {
                    ps.push(t.nulls);
                    cur = t.next.as_deref();
                }
                return (ret, ps);
            }
        }
        panic!("no typedef found in {:?}", src);
    }

    // Block param annotated, both spellings.
    let (ret, ps) = typedef_block("typedef void (^B)(nonnull NPString *s);\n");
    println!("block param prefix -> ret={:?} params={:?}", ret, ps);
    assert_eq!(ps, vec![Nullability::Nonnull], "block param prefix not recorded");

    let (ret, ps) = typedef_block("typedef void (^B)(NPString * _Nullable s);\n");
    println!("block param postfix -> ret={:?} params={:?}", ret, ps);
    assert_eq!(ps, vec![Nullability::Nullable], "block param postfix not recorded");

    // Unannotated stays unspecified (no false positive).
    let (_ret, ps) = typedef_block("typedef void (^B)(NPString *s);\n");
    println!("block param plain -> params={:?}", ps);
    assert_eq!(ps, vec![Nullability::Unspecified], "plain block param annotated");
}

#[test]
fn block_literal_return_annotation_is_recorded() {
    let mut p = Parser::new("typedef nullable NPString * (^Get)(void);\n");
    let unit = p.parse_translation_unit().expect("unit");
    let mut found = None;
    for d in &unit.decls {
        if let CstDeclData::Typedef { alias_type: Some(at), .. } = &d.data {
            found = Some(at.nulls);
        }
    }
    println!("block return -> {:?}", found);
    assert_eq!(found, Some(Nullability::Nullable), "block return not recorded");
}

#[test]
fn double_ptr_postfix_annotates_middle_level() {
    // `NPError * _Nullable * out` — the postfix word follows the FIRST star, so
    // it annotates that (middle) level; the outer stays unannotated. This is
    // the out-param shape: the error object is nullable, the out-pointer is
    // always valid because the callee writes through it.
    let mut p = Parser::new("void f(NPError * _Nullable * out);\n");
    let unit = p.parse_translation_unit().expect("unit");
    let mut found = None;
    for d in &unit.decls {
        if let CstDeclData::Function { params, .. } = &d.data {
            if let Some(pp) = params.as_deref() {
                if let Some(ref pt) = pp.par_type {
                    let outer = pt.nulls;
                    let middle = pt.subtype.as_ref()
                        .map(|s| s.nulls)
                        .unwrap_or(Nullability::Unspecified);
                    found = Some((outer, middle));
                }
            }
        }
    }
    println!("double-ptr postfix -> {:?}", found);
    assert_eq!(
        found,
        Some((Nullability::Unspecified, Nullability::Nullable)),
        "postfix between stars must mark the MIDDLE level: {:?}",
        found
    );
}

#[test]
fn double_ptr_prefix_is_rejected_like_clang() {
    // `nullable NPError * *` — which level did the author mean? clang refuses
    // to guess ("nullability specifier cannot be applied to non-pointer type
    // 'Widget'", verified against the host SDK); jeti matches that and points
    // at the postfix spelling that annotates a chosen level.
    let mut p = Parser::new("void f(nullable NPError * * out);\n");
    p.parse_translation_unit();
    assert!(p.has_error(), "prefix nullability on a double pointer must be rejected");
    assert!(
        p.last_error().contains("cannot be applied to non-pointer type"),
        "wrong message: {}",
        p.last_error()
    );
}

#[test]
fn annotation_word_as_a_declarator_name_still_parses() {
    // The lookahead guard: `nullable foo = 5;` declares a variable of type
    // `nullable` named `foo`. Without the guard the annotation is eaten and the
    // declaration fails.
    let mut p = Parser::new("nullable foo = 5;\n");
    let unit = p.parse_translation_unit();
    assert!(!p.has_error(), "broke legal C: {}", p.last_error());
    assert_eq!(unit.unwrap().decls.len(), 1);
}

/// Extract ivars from every class in the unit: (name, nulls) pairs, plus the
/// parser's error state. Ivars are the position under test here: clang
/// (probe-verified) accepts nullability there only in the postfix spelling
/// (`NPString * _Nullable x`) and rejects the prefix form.
fn ivars_of(src: &str) -> (Vec<(String, Nullability)>, bool, String) {
    let mut p = Parser::new(src);
    let unit = p.parse_translation_unit().expect("unit");
    let mut out = Vec::new();
    for d in &unit.decls {
        if let CstDeclData::Class { ivars, .. } = &d.data {
            for v in ivars {
                if let CstDeclData::Ivar { ivar_type, .. } = &v.data {
                    out.push((
                        v.name.clone().unwrap_or_default(),
                        ivar_type
                            .as_ref()
                            .map(|t| t.nulls)
                            .unwrap_or(Nullability::Unspecified),
                    ));
                }
            }
        }
    }
    (out, p.has_error(), p.last_error().to_string())
}

#[test]
fn ivar_postfix_nullable_is_recorded() {
    // The form ObjC allows in ivar position — and the one Foundation headers
    // use for explicit region opt-out (`NPString * _Nullable x`).
    let (got, err, msg) =
        ivars_of("@interface S : NPObject {\n    NPString * _Nullable x;\n}\n@end\n");
    println!("ivar postfix nullable -> {:?} err={}", got, err);
    assert!(!err, "postfix nullability in ivar must parse: {}", msg);
    assert_eq!(
        got,
        vec![("x".into(), Nullability::Nullable)],
        "postfix not recorded: {:?}",
        got
    );
}

#[test]
fn ivar_postfix_nonnull_is_recorded() {
    let (got, err, msg) =
        ivars_of("@interface S : NPObject {\n    NPString * _Nonnull x;\n}\n@end\n");
    println!("ivar postfix nonnull -> {:?} err={}", got, err);
    assert!(!err, "postfix _Nonnull in ivar must parse: {}", msg);
    assert_eq!(
        got,
        vec![("x".into(), Nullability::Nonnull)],
        "postfix not recorded: {:?}",
        got
    );
}

#[test]
fn ivar_prefix_bare_is_rejected() {
    // clang: "unknown type name 'nullable'". Jeti says the same thing more
    // usefully: one targeted error, and the declaration still parses with the
    // annotation's meaning preserved (recovery, no cascade).
    let (got, err, msg) =
        ivars_of("@interface S : NPObject {\n    nullable NPString *x;\n}\n@end\n");
    println!("ivar prefix bare -> {:?} err={} msg={}", got, err, msg);
    assert!(err, "prefix nullability in ivar must be rejected");
    assert!(
        msg.contains("follow the pointer"),
        "wrong message: {}",
        msg
    );
    assert_eq!(
        got,
        vec![("x".into(), Nullability::Nullable)],
        "recovery lost the ivar: {:?}",
        got
    );
}

#[test]
fn ivar_prefix_underscore_is_rejected() {
    // clang: "nullability specifier '_Nullable' cannot be applied to
    // non-pointer type 'NSString'". Same targeted rejection as the bare form.
    let (got, err, msg) =
        ivars_of("@interface S : NPObject {\n    _Nullable NPString *x;\n}\n@end\n");
    println!("ivar prefix underscore -> {:?} err={} msg={}", got, err, msg);
    assert!(err, "prefix _Nullable in ivar must be rejected");
    assert!(
        msg.contains("follow the pointer"),
        "wrong message: {}",
        msg
    );
    assert_eq!(
        got,
        vec![("x".into(), Nullability::Nullable)],
        "recovery lost the ivar: {:?}",
        got
    );
}

#[test]
fn ivar_region_default_still_applies() {
    // The region's unannotated-pointer default must keep working for ivars —
    // the rejection only targets EXPLICIT prefix annotations.
    let (got, err, msg) = ivars_of(
        "NP_ASSUME_NONNULL_BEGIN\n\
         @interface R : NPObject {\n\
         NPString *x;\n\
         }\n\
         @end\n\
         NP_ASSUME_NONNULL_END\n",
    );
    println!("ivar region default -> {:?} err={}", got, err);
    assert!(!err, "region default broke ivar parsing: {}", msg);
    assert_eq!(
        got,
        vec![("x".into(), Nullability::Nonnull)],
        "region default NOT applied to ivar: {:?}",
        got
    );
}

#[test]
fn c_struct_field_named_nullable_still_parses() {
    // C superset: struct fields parse through `parse_type_full` (never with
    // `annotating` set), so a field NAMED nullable is untouched by the ivar
    // rule, and the ivar flag must not leak into struct parsing.
    let mut p = Parser::new("struct S { int nullable; };\n");
    let unit = p.parse_translation_unit();
    assert!(!p.has_error(), "broke legal C struct: {}", p.last_error());
    assert_eq!(unit.unwrap().decls.len(), 1);
}
