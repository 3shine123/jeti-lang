// Dead code is a build failure, not a warning: a function nobody calls is
// either a bug or a leftover, and both should surface at compile time rather
// than rot unnoticed. Mark intentional exceptions with #[allow(dead_code)]
// and a comment saying who will use it.
#![deny(dead_code)]
//! C17 6.7.2.1 struct/union layout — computed by the compiler, consumed by
//! the LSP hover cards (clangd-parity: "Size: 48 bytes, alignment 8 bytes",
//! per-field offset/size lines) and available to any pass that must know a
//! struct's memory shape without asking the backend.
//!
//! The ABI target is the LP64 hosted model the compiler already assumes for
//! pointer width (see the checker's cast family): 64-bit pointers, 4-byte
//! int/float, 8-byte long/double, 1-byte char/bool. Apple arm64 matches this
//! for the types jeti programs use (`x86_64` and `aarch64` differ only in
//! cases jeti code does not reach: long double = 16 bytes there).
//!
//! `__attribute__((packed))` on an aggregate drops every internal padding and
//! caps the aggregate's alignment at 1 (gcc/clang semantics, probe-measured).

use jeti_ast::{AstDecl, AstDeclData, AstType};
use jeti_cst::TypePrim;
use std::collections::HashMap;

/// One field's computed layout, as the hover card spells it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldLayout {
    pub name: String,
    /// Declared type spelling, C order (`int[2][3]`, `unsigned int`, `int (*)(int)`).
    pub type_spelling: String,
    /// Byte offset from the struct start.
    pub offset: u64,
    /// In-memory size: bits for bit-fields, bytes otherwise.
    pub size: u64,
    /// Bit-field width (`unsigned flags : 3` → 3). `None` = plain field.
    pub bit_width: Option<u64>,
    /// Total size of the field's type in bits — for a bit-field this is the
    /// underlying storage unit (`unsigned` → 32), matching clang's
    /// "3 bits (+29 bits padding)" rendering.
    pub storage_bits: Option<u64>,
}

/// A whole aggregate's layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructLayout {
    pub name: String,
    pub is_union: bool,
    pub size: u64,
    pub alignment: u64,
    pub fields: Vec<FieldLayout>,
    /// `true` when an attribute made the fields packed.
    pub is_packed: bool,
}

/// LP64 primitive sizes/alignments (bytes).
fn primitive_size(prim: TypePrim) -> u64 {
    match prim {
        TypePrim::Char => 1,
        TypePrim::Bool => 1,
        TypePrim::Short => 2,
        TypePrim::Int => 4,
        TypePrim::Long => 8,
        TypePrim::LongLong => 8,
        TypePrim::Float => 4,
        TypePrim::Double => 8,
        TypePrim::Signed => 4,
        TypePrim::Unsigned => 4,
        // LP64: all object/class pointers are 8. SEL is 8 too (a pointer-sized
        // handle); id/Class/instancetype lower to NPObject */NPClass *.
        TypePrim::Id | TypePrim::Class | TypePrim::Sel | TypePrim::Instancetype => 8,
        TypePrim::Void => 1, // only reachable via `void *`, handled by is_pointer
        // Named/tagged spellings resolve through the registry; the fallback
        // here is for resolved-away cases and stays conservative.
        _ => 8,
    }
}

fn primitive_align(prim: TypePrim) -> u64 {
    primitive_size(prim)
}

/// Size/alignment of one type in bytes. `layout` resolves user-defined
/// aggregates and typedefs (recursion guarded by the visitor — a struct may
/// only contain pointers to itself, so the direct-reference cycle C forbids
/// cannot arise here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeLayout {
    pub size: u64,
    pub alignment: u64,
}

struct Engine<'a> {
    structs: &'a HashMap<String, StructLayout>,
    typedefs: &'a HashMap<String, AstType>,
}

impl<'a> Engine<'a> {
    fn type_layout(&self, t: &AstType, depth: u32) -> TypeLayout {
        if depth > 32 {
            return TypeLayout { size: 8, alignment: 8 };
        }
        // Arrays: element size × count (first array dimension only — the AST
        // models `[2][3]` as array-of-array through `subtype`, which the
        // recursion below folds).
        if t.is_array {
            let inner = t.subtype.as_deref().map(|s| self.type_layout(s, depth + 1));
            let elem = inner.unwrap_or_else(|| {
                // No subtype recorded: the element is the type's own primitive.
                TypeLayout { size: primitive_size(t.prim), alignment: primitive_align(t.prim) }
            });
            let n = if t.array_size > 0 { t.array_size as u64 } else { 0 };
            return TypeLayout {
                size: elem.size * n.max(1),
                alignment: elem.alignment,
            };
        }
        // A named struct/union spelling resolves through the registry.
        if t.is_struct {
            if let Some(name) = t.name.as_deref() {
                if let Some(l) = self.structs.get(name) {
                    return TypeLayout { size: l.size, alignment: l.alignment };
                }
            }
            // Unresolved (forward-only) — clang requires the definition at
            // use; the hover card treats it as unknown and skips size lines.
            return TypeLayout { size: 0, alignment: 0 };
        }
        // Pointers: LP64, 8 bytes regardless of pointee.
        if t.is_pointer || t.is_block || t.is_fn_ptr {
            return TypeLayout { size: 8, alignment: 8 };
        }
        // typedef alias → the aliased type (one hop; typedefs never chain
        // deeply in real code, and the depth guard bounds pathology).
        if t.prim == TypePrim::Named {
            if let Some(name) = t.name.as_deref() {
                if let Some(at) = self.typedefs.get(name) {
                    return self.type_layout(at, depth + 1);
                }
            }
            return TypeLayout { size: 8, alignment: 8 };
        }
        TypeLayout {
            size: primitive_size(t.prim),
            alignment: primitive_align(t.prim),
        }
    }
}

/// The C spelling of a type for the hover card, in declaration order
/// (`unsigned int`, `int[2][3]`, `int (*)(int)`).
pub fn type_spelling(t: &AstType) -> String {
    if t.is_fn_ptr {
        let ret = t.subtype.as_ref().map(|s| type_spelling(s)).unwrap_or_else(|| "int".into());
        let params = t.block_params.as_ref()
            .map(|p| {
                let mut parts = Vec::new();
                let mut cur: Option<&AstType> = Some(&**p);
                while let Some(pt) = cur {
                    parts.push(type_spelling(pt));
                    cur = pt.next.as_deref();
                }
                if parts.is_empty() { "void".to_string() } else { parts.join(", ") }
            })
            .unwrap_or_else(|| "void".into());
        let name = t.block_name.clone().unwrap_or_default();
        return format!("{ret} (*{name})({params})");
    }
    let mut s = String::new();
    if t.is_const && !t.is_pointer { s.push_str("const "); }
    let kw = match t.prim {
        TypePrim::Void => "void",
        TypePrim::Char => "char",
        TypePrim::Short => "short",
        TypePrim::Int => "int",
        TypePrim::Long => "long",
        TypePrim::LongLong => "long long",
        TypePrim::Float => "float",
        TypePrim::Double => "double",
        TypePrim::Bool => "BOOL",
        TypePrim::Signed => "signed",
        TypePrim::Unsigned => "unsigned",
        TypePrim::Id => "id",
        TypePrim::Class => "Class",
        TypePrim::Sel => "SEL",
        TypePrim::Instancetype => "instancetype",
        TypePrim::Named => {
            let name = t.name.as_deref().unwrap_or("");
            if t.is_struct {
                let tag = t.tag.keyword();
                s.push_str(tag);
                s.push(' ');
                name
            } else {
                name
            }
        }
        TypePrim::Param => t.name.as_deref().unwrap_or("T"),
    };
    s.push_str(kw);
    if !t.type_args.is_empty() {
        let args = t.type_args.iter().map(|a| type_spelling(a)).collect::<Vec<_>>().join(", ");
        s.push_str(&format!("<{args}>"));
    }
    if t.is_array {
        // Multi-dimensional: `[2][3]` is array-of-array — collect ALL dims
        // down the subtype chain, spell the base once, then every dim in
        // order (clang's `int[2][3]`).
        let mut dims: Vec<String> = Vec::new();
        let mut cur: &AstType = t;
        while cur.is_array {
            if cur.array_size > 0 {
                dims.push(format!("[{}]", cur.array_size));
            } else if let Some(n) = cur.array_size_name.as_deref() {
                dims.push(format!("[{n}]"));
            } else {
                dims.push("[]".to_string());
            }
            match cur.subtype.as_deref() {
                Some(sub) if sub.is_array => cur = sub,
                Some(sub) => {
                    // last element type: spell its base (recursion handles
                    // qualifiers/pointers, but strip any dims it would add —
                    // there are none beyond the chain by construction)
                    s.push_str(&type_spelling(sub));
                    break;
                }
                None => break,
            }
        }
        // The parser wraps `name[size1][size2]` outermost-LAST (the first
        // bracket is the innermost array), so the collected order is
        // right-to-left; C's spelling reads left-to-right — reverse.
        dims.reverse();
        if dims.is_empty() {
            dims.push("[]".to_string());
        }
        for d in dims {
            s.push_str(&d);
        }
        if t.is_pointer {
            s.push_str(" *");
            if t.is_const { s.push_str(" const"); }
        }
        return s;
    }
    if t.is_pointer {
        s.push_str(" *");
        if t.is_const { s.push_str(" const"); }
    }
    s
}

/// A bit-field's declared width lives where the parser put `flags : 3` —
/// the CST folds it into the field's `attributes` as the raw trailing
/// spelling; recover it from the raw attribute text (`": 3"`).
fn bit_width_of(decl: &AstDecl) -> Option<u64> {
    for a in &decl.attributes {
        let a = a.trim();
        if let Some(rest) = a.strip_prefix(':') {
            return rest.trim().parse().ok();
        }
        // The parser may spell the whole declarator tail; accept `flags : 3`
        // and bare `: 3`.
        if let Some(pos) = a.rfind(':') {
            let w = a[pos + 1..].trim();
            if !w.is_empty() && w.chars().all(|c| c.is_ascii_digit()) {
                return w.parse().ok();
            }
        }
    }
    None
}

fn is_packed(decl: &AstDecl) -> bool {
    decl.attributes.iter().any(|a| a.contains("packed"))
}

/// Compute the layout of one `struct`/`union` declaration with a body.
/// Anonymous members (unnamed nested aggregates) occupy space without a
/// name — recorded with an empty `name`, matching clangd's card.
pub fn layout_of(decl: &AstDecl, structs: &HashMap<String, StructLayout>, typedefs: &HashMap<String, AstType>) -> Option<StructLayout> {
    let AstDeclData::Aggregate { fields, is_union } = &decl.data else { return None };
    let name = decl.name.clone().unwrap_or_default();
    let packed = is_packed(decl);
    let engine = Engine { structs, typedefs };

    let mut out_fields: Vec<FieldLayout> = Vec::new();
    let mut offset: u64 = 0;
    let mut alignment: u64 = 1;
    let mut size: u64 = 0;

    for f in fields {
        let AstDeclData::Ivar { ivar_type, .. } = &f.data else { continue };
        let Some(ty) = ivar_type.as_deref() else { continue };
        let spelling = type_spelling(ty);

        if let Some(bits) = bit_width_of(f) {
            // Bit-field (C17 6.7.2.1p9-11): allocated within a storage unit
            // of the declared type's width, not byte-aligned.
            let tl = engine.type_layout(ty, 0);
            let storage_bits = tl.size * 8;
            // A zero-width bit-field forces alignment to the unit boundary.
            if bits == 0 {
                offset = round_up(offset, tl.alignment);
                out_fields.push(FieldLayout {
                    name: String::new(),
                    type_spelling: spelling,
                    offset,
                    size: 0,
                    bit_width: Some(0),
                    storage_bits: Some(storage_bits),
                });
                alignment = alignment.max(tl.alignment);
                continue;
            }
            // Does it fit in the current unit? (Union: always a fresh unit.)
            let unit_start = round_down(offset, tl.alignment);
            let used_bits = (offset - unit_start) * 8;
            let unit_bits = tl.alignment * 8;
            let fits = !*is_union && used_bits + bits <= unit_bits;
            let start_bit = if fits { unit_start * 8 + used_bits } else { offset * 8 };
            let field_offset = start_bit / 8;
            out_fields.push(FieldLayout {
                name: f.name.clone().unwrap_or_default(),
                type_spelling: spelling,
                offset: field_offset,
                size: bits,
                bit_width: Some(bits),
                storage_bits: Some(storage_bits),
            });
            // Advance past the consumed bits, rounded up to whole units.
            let end_bit = start_bit + bits;
            let end_unit = end_bit.div_ceil(unit_bits);
            offset = end_unit * unit_bits / 8 * 1;
            offset = end_unit * unit_bits / 8;
            offset = end_unit * (unit_bits / 8);
            alignment = alignment.max(tl.alignment);
            size = size.max(offset);
            continue;
        }

        let tl = engine.type_layout(ty, 0);
        if tl.size == 0 && tl.alignment == 0 {
            // Unresolved type: record the field without layout numbers.
            out_fields.push(FieldLayout {
                name: f.name.clone().unwrap_or_default(),
                type_spelling: spelling,
                offset: 0,
                size: 0,
                bit_width: None,
                storage_bits: None,
            });
            continue;
        }
        let falign = if packed { 1 } else { tl.alignment };
        if *is_union {
            out_fields.push(FieldLayout {
                name: f.name.clone().unwrap_or_default(),
                type_spelling: spelling,
                offset: 0,
                size: tl.size,
                bit_width: None,
                storage_bits: None,
            });
            size = size.max(tl.size);
            alignment = alignment.max(falign);
        } else {
            offset = round_up(offset, falign);
            out_fields.push(FieldLayout {
                name: f.name.clone().unwrap_or_default(),
                type_spelling: spelling,
                offset,
                size: tl.size,
                bit_width: None,
                storage_bits: None,
            });
            offset += tl.size;
            alignment = alignment.max(falign);
            size = offset;
        }
    }

    // Tail padding to the aggregate's alignment (packed → 1, no tail pad).
    let total = if packed {
        if *is_union { size } else { size }
    } else {
        round_up(size.max(1), alignment)
    };
    // An empty struct is 0 in C17 (gcc extension: 0 with align 1); clang gives
    // struct {} a size of 0 on C. Keep 0.
    Some(StructLayout {
        name,
        is_union: *is_union,
        size: total,
        alignment,
        fields: out_fields,
        is_packed: packed,
    })
}

fn round_up(v: u64, a: u64) -> u64 {
    if a == 0 { return v; }
    v.div_ceil(a) * a
}

fn round_down(v: u64, a: u64) -> u64 {
    if a == 0 { return v; }
    v / a * a
}

/// Walk a declaration list collecting every aggregate's layout, plus the
/// typedef alias map (`typedef struct X Y;` — hover on `Y` shows X's card).
/// Namespaced/qualified names are keyed by their RAW spelling as written; the
/// caller (LSP index) resolves spellings before lookup.
pub fn collect_layouts(decls: &[AstDecl]) -> (HashMap<String, StructLayout>, HashMap<String, AstType>) {
    let mut structs: HashMap<String, StructLayout> = HashMap::new();
    let mut typedefs: HashMap<String, AstType> = HashMap::new();
    collect_into(decls, &mut structs, &mut typedefs);
    (structs, typedefs)
}

fn collect_into(decls: &[AstDecl], structs: &mut HashMap<String, StructLayout>, typedefs: &mut HashMap<String, AstType>) {
    for d in decls {
        match &d.data {
            AstDeclData::Aggregate { .. } => {
                if let Some(l) = layout_of(d, structs, typedefs) {
                    if !l.name.is_empty() {
                        structs.insert(l.name.clone(), l);
                    }
                }
                // Nested field decls carry nothing further to collect.
                if let AstDeclData::Aggregate { fields, .. } = &d.data {
                    collect_into(fields, structs, typedefs);
                }
            }
            AstDeclData::Typedef { aliased_type: Some(at), .. } => {
                if let Some(alias) = &d.name {
                    typedefs.insert(alias.clone(), (**at).clone());
                }
            }
            AstDeclData::Class { methods, ivars, impl_vars, properties, .. } => {
                // A class's ivars form an implicit aggregate; hover over the
                // class name can show its size the same way.
                let synth = AstDecl {
                    kind: jeti_ast::AstDeclKind::Struct,
                    line: d.line,
                    col: d.col,
                    name: d.name.clone(),
                    data: AstDeclData::Aggregate {
                        fields: ivars.iter().chain(impl_vars.iter()).cloned().collect(),
                        is_union: false,
                    },
                    attributes: Vec::new(),
                    end_line: d.end_line,
                    end_col: d.end_col,
                };
                if let Some(l) = layout_of(&synth, structs, typedefs) {
                    if !l.name.is_empty() {
                        structs.insert(l.name.clone(), l);
                    }
                }
                let _ = (methods, properties);
                collect_into(methods, structs, typedefs);
            }
            AstDeclData::Namespace(inner) => collect_into(inner, structs, typedefs),
            _ => {}
        }
    }
}

/// The clangd-style hover body for a struct: "Size: N bytes, alignment M
/// bytes" plus the field offset lines (one per field, "offset size name —
/// type"), all ASCII/English (the cards the editor shows must match clangd's
/// language, not carry prose).
pub fn hover_body(l: &StructLayout) -> String {
    let kind = if l.is_union { "union" } else { "struct" };
    let mut s = format!("Size: {} bytes, alignment {} bytes", l.size, l.alignment);
    if l.is_packed {
        s.push_str(" (packed)");
    }
    s.push_str(&format!("\n\n{kind} {name} {{ ... }}", name = l.name));
    if !l.fields.is_empty() {
        s.push_str("\n\n");
        let mut lines: Vec<String> = Vec::new();
        for f in &l.fields {
            if let Some(bits) = f.bit_width {
                let storage = f.storage_bits.unwrap_or(0);
                let pad = storage.saturating_sub(bits);
                let size_txt = if pad > 0 {
                    format!("{bits} bits (+{pad} bits padding)")
                } else {
                    format!("{bits} bits")
                };
                lines.push(format!(
                    "{:>4}  {:<28} {} : {}",
                    f.offset, size_txt, if f.name.is_empty() { "<anon>" } else { &f.name }, f.type_spelling
                ));
            } else {
                lines.push(format!(
                    "{:>4}  {:<28} {} : {}",
                    f.offset, format!("{} bytes", f.size), if f.name.is_empty() { "<anon>" } else { &f.name }, f.type_spelling
                ));
            }
        }
        s.push_str(&lines.join("\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ivar(name: &str, ty: AstType, attrs: Vec<String>) -> AstDecl {
        AstDecl {
            kind: jeti_ast::AstDeclKind::Ivar,
            line: 1, col: 1, name: Some(name.into()),
            data: AstDeclData::Ivar { ivar_sym: None, ivar_type: Some(Box::new(ty)), is_weak: false },
            attributes: attrs,
            end_line: 0, end_col: 0,
        }
    }

    fn aggregate(name: &str, fields: Vec<AstDecl>, attrs: Vec<String>, is_union: bool) -> AstDecl {
        AstDecl {
            kind: jeti_ast::AstDeclKind::Struct,
            line: 1, col: 1, name: Some(name.into()),
            data: AstDeclData::Aggregate { fields, is_union },
            attributes: attrs,
            end_line: 0, end_col: 0,
        }
    }

    fn prim(p: TypePrim) -> AstType { AstType::new(p) }

    fn named_struct(name: &str) -> AstType {
        let mut t = AstType::new(TypePrim::Named);
        t.is_struct = true;
        t.name = Some(name.into());
        t
    }

    fn ptr_to(t: AstType) -> AstType {
        let mut p = AstType::new(t.prim);
        p.is_pointer = true;
        p.name = t.name.clone();
        p.subtype = Some(Box::new(t));
        p
    }

    #[test]
    fn simple_struct_matches_clang_abi() {
        // struct FsWidget { int wid; } — 4 bytes, align 4.
        let d = aggregate("FsWidget", vec![ivar("wid", prim(TypePrim::Int), vec![])], vec![], false);
        let (m, td) = collect_layouts(&[d]);
        let l = &m["FsWidget"];
        assert_eq!((l.size, l.alignment), (4, 4));
        assert_eq!(l.fields[0].offset, 0);
    }

    #[test]
    fn padding_and_alignment_match_clang() {
        // struct { char c; int v; } — clang: 8 bytes, align 4, v@4.
        let d = aggregate("S", vec![
            ivar("c", prim(TypePrim::Char), vec![]),
            ivar("v", prim(TypePrim::Int), vec![]),
        ], vec![], false);
        let (m, _) = collect_layouts(&[d]);
        let l = &m["S"];
        assert_eq!((l.size, l.alignment), (8, 4));
        assert_eq!(l.fields[1].offset, 4);
    }

    #[test]
    fn packed_drops_padding() {
        // __attribute__((packed)): size 5, align 1.
        let d = aggregate("P", vec![
            ivar("c", prim(TypePrim::Char), vec![]),
            ivar("v", prim(TypePrim::Int), vec![]),
        ], vec!["packed".into()], false);
        let (m, _) = collect_layouts(&[d]);
        let l = &m["P"];
        assert_eq!((l.size, l.alignment), (5, 1));
        assert_eq!(l.fields[1].offset, 1);
    }

    #[test]
    fn multidim_array_field() {
        // int cells[2][3] — 24 bytes.
        // The parser wraps `cells[2][3]` outermost-LAST: the outer type is
        // `[3]` (array of 2 arrays of 3 ints). Match that shape here — the
        // spelling must come out `int[2][3]` (declaration order), which is
        // why the engine reverses the collected dims.
        let mut arr = AstType::new(TypePrim::Int);
        arr.is_array = true;
        arr.array_size = 3;
        arr.subtype = Some(Box::new({
            let mut a = AstType::new(TypePrim::Int);
            a.is_array = true;
            a.array_size = 2;
            a
        }));
        let d = aggregate("W", vec![ivar("cells", arr, vec![])], vec![], false);
        let (m, _) = collect_layouts(&[d]);
        assert_eq!(m["W"].fields[0].size, 24);
        assert_eq!(type_spelling(&{
            let mut arr = AstType::new(TypePrim::Int);
            arr.is_array = true;
            arr.array_size = 3;
            arr.subtype = Some(Box::new({
                let mut a = AstType::new(TypePrim::Int);
                a.is_array = true;
                a.array_size = 2;
                a
            }));
            arr
        }), "int[2][3]");
    }

    #[test]
    fn bit_field_matches_clang_shape() {
        // struct { int wid; unsigned flags : 3; int in; }
        // clang (LP64): wid@0, flags@4 (3 bits of a 4-byte unit), in@8, size 12.
        let d = aggregate("B", vec![
            ivar("wid", prim(TypePrim::Int), vec![]),
            ivar("flags", { let mut t = prim(TypePrim::Unsigned); t }, vec![": 3".into()]),
            ivar("in", prim(TypePrim::Int), vec![]),
        ], vec![], false);
        let (m, _) = collect_layouts(&[d]);
        let l = &m["B"];
        assert_eq!(l.fields[1].bit_width, Some(3));
        assert_eq!(l.fields[1].offset, 4);
        assert_eq!(l.fields[2].offset, 8);
        assert_eq!(l.size, 12);
    }

    #[test]
    fn union_size_is_max_member() {
        // union { int i; float f; char b[4]; } — 4 bytes, align 4.
        let mut arr = AstType::new(TypePrim::Char);
        arr.is_array = true;
        arr.array_size = 4;
        let d = aggregate("U", vec![
            ivar("i", prim(TypePrim::Int), vec![]),
            ivar("f", prim(TypePrim::Float), vec![]),
            ivar("b", arr, vec![]),
        ], vec![], true);
        let (m, _) = collect_layouts(&[d]);
        let l = &m["U"];
        assert!(l.is_union);
        assert_eq!((l.size, l.alignment), (4, 4));
        assert_eq!(l.fields[2].offset, 0);
    }

    #[test]
    fn nested_struct_and_pointer_to_self() {
        // struct Inner { int a; int b; }; struct Outer { struct Inner i; int tag; };
        // → Outer: i@0 (8), tag@8, size 12, align 4.
        let inner = aggregate("Inner", vec![
            ivar("a", prim(TypePrim::Int), vec![]),
            ivar("b", prim(TypePrim::Int), vec![]),
        ], vec![], false);
        // struct Node { int v; struct Node *next; } — 16 bytes (pointer).
        let node = aggregate("Node", vec![
            ivar("v", prim(TypePrim::Int), vec![]),
            ivar("next", ptr_to(named_struct("Node")), vec![]),
        ], vec![], false);
        let outer = aggregate("Outer", vec![
            ivar("i", named_struct("Inner"), vec![]),
            ivar("tag", prim(TypePrim::Int), vec![]),
        ], vec![], false);
        let (m, _) = collect_layouts(&[inner, node, outer]);
        assert_eq!(m["Outer"].fields[0].size, 8);
        assert_eq!(m["Outer"].fields[1].offset, 8);
        assert_eq!(m["Outer"].size, 12);
        assert_eq!(m["Node"].size, 16, "self-pointer must not recurse into the size");
    }

    #[test]
    fn typedef_alias_resolves() {
        // typedef struct Inner In; In x; — 8 bytes.
        let inner = aggregate("Inner", vec![
            ivar("a", prim(TypePrim::Int), vec![]),
            ivar("b", prim(TypePrim::Int), vec![]),
        ], vec![], false);
        let mut alias_ty = AstType::new(TypePrim::Named);
        alias_ty.is_struct = true;
        alias_ty.name = Some("Inner".into());
        let td = AstDecl {
            kind: jeti_ast::AstDeclKind::Typedef,
            line: 1, col: 1, name: Some("In".into()),
            data: AstDeclData::Typedef { aliased_type: Some(Box::new(alias_ty)), struct_fields: Vec::new() },
            attributes: Vec::new(),
            end_line: 0, end_col: 0,
        };
        // `In x` — var whose type says Named "In" (no is_struct), resolved via typedefs.
        let use_decl = aggregate("UseHolder", vec![ivar("x", {
            let mut t = AstType::new(TypePrim::Named);
            t.name = Some("In".into());
            t
        }, vec![])], vec![], false);
        let (m, tds) = collect_layouts(&[inner, td, use_decl]);
        assert_eq!(tds.contains_key("In"), true);
        // `In` resolves through typedefs → Inner's 8 bytes.
        let l = &m["UseHolder"];
        assert_eq!(l.fields[0].size, 8);
        assert_eq!(m["Inner"].size, 8);
    }
}
