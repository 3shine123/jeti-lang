#!/usr/bin/env bash
# build-foundation-lib.sh — build the precompiled Foundation library (Plan A).
#
#   ./tools/build-foundation-lib.sh [outdir]        # default: target/foundation
#
# Per-TU compilation (docs/plans/stable_slots_plan.md §11, plan A): every Foundation
# `.jeti` is compiled as its OWN translation unit — i.e. as a main file — so R2
# ownership makes each owned class's metadata STRONG automatically. No
# -fstrong-metadata anywhere.
#
#   jetic app.jeti -I include -L<outdir> -ljetifoundation -o app
#
# The client imports `Foundation.hje` — the DECLARATION-ONLY umbrella (per the
# project's .hje = declarations / .jeti = implementations convention).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

jetic="${JETIC:-}"
if [[ -z "$jetic" ]]; then
    for cand in target/debug/jetic target/release/jetic; do
        if [[ -x "$cand" ]]; then jetic="$cand"; break; fi
    done
fi
if [[ -z "$jetic" || ! -x "$jetic" ]]; then
    echo "error: jetic not found (run 'cargo build', or set JETIC=/path/to/jetic)" >&2
    exit 2
fi

outdir="${1:-target/foundation}"
mkdir -p "$outdir"
lib="$outdir/libjetifoundation.a"

# Mach-O prepends `_` to every C symbol; ELF uses the name as written.
sym_prefix=""
case "$(uname -s)" in
    Darwin) sym_prefix="_" ;;
esac

# Weakness is platform-specific in nm output:
#   * Mach-O (Darwin): plain nm shows a weak definition as 'D'/'T' anyway, so
#     `nm -m` and its "weak external" marker are required.
#   * ELF (Linux): weak symbols carry 'W'/'V'/'w'/'v'.
nm_is_weak() {
    local obj="$1" sym="$2"
    case "$(uname -s)" in
        Darwin)
            nm -m "$obj" 2>/dev/null | grep -F -- " $sym" | grep -q "weak external"
            ;;
        *)
            nm "$obj" 2>/dev/null | grep -F -- " $sym" \
                | grep -qE '^[0-9a-fA-F]+[[:space:]]+[WwVv][[:space:]]'
            ;;
    esac
}

check_strong() {
    local obj="$1" cls="$2"
    for member in "JETI_VTABLE_\$_${cls}" "JETI_META_VTABLE_\$_${cls}_inst" "JETI_GETCLASS_\$_${cls}"; do
        local sym="${sym_prefix}${member}"
        if ! nm "$obj" 2>/dev/null | grep -qF -- " $sym"; then
            echo "  error: symbol $sym not found in $obj" >&2
            exit 1
        fi
        if nm_is_weak "$obj" "$sym"; then
            echo "  error: $sym is WEAK — R2 ownership did not apply (is $cls really implemented in $(basename "$obj" .o).jeti?)" >&2
            exit 1
        fi
        echo "  ok: $sym"
    done
}

echo "[1/4] transpile + compile each Foundation TU (wrapper = decl surface + implementation)"
# Each TU is a generated WRAPPER: the full declaration surface
# (Foundation.hje, the declaration-only umbrella) first, then the
# implementation .jeti's own text — the
# @implementation lands in the MAIN FILE (not via #import), so R2 ownership
# still applies and this TU's metadata is strong. The full surface gives
# every TU the same shared vtable segment as a client (same __sig), and stub
# references to sibling implementations resolve from the archive at final
# link. The source .jeti files stay UNPOLLUTED — self-contained TUs that inline
# them keep exactly the declaration surface they asked for.
# Note: NPObject.jeti imports "NPObject.hje" in quoted form; the extra
# -I include/Foundation keeps that resolvable from the wrapper's location.
objs=()
tus="$outdir/_tus"
rm -rf "$tus"   # stale wrappers from an interrupted earlier run must not survive
mkdir -p "$tus"
rm -f "$outdir"/*.o   # stale objects from an earlier run must not linger into [3/4]
for gm in include/Foundation/*.jeti; do
    name="$(basename "$gm" .jeti)"
    # Skip the self-contained umbrella (Foundation.jeti): it is not a class —
    # it has no vtable to verify, and archiving it would re-inline every
    # implementation into the library, defeating the per-TU build.
    # Skip NPTask.jeti for the same reason: `NPTask<T>` is the runtime task
    # HANDLE (route A), not a class — no implementation, no vtable
    # (docs/plans/async_nptask_plan.md §Foundation 壳层).
    [[ "$name" == "Foundation" || "$name" == "NPTask" ]] && continue
    wrap="$tus/$name.jeti"
    { echo '#import <Foundation/Foundation.hje>'; cat "$gm"; } > "$wrap"
    c="$outdir/$name.c"
    o="$outdir/$name.o"
    "$jetic" -rewrite-jeti "$wrap" -o "$c" -I include -I include/Foundation
    clang -c -w "$c" -o "$o" -I include
    objs+=("$o")
    echo "  $name"
done

echo "[2/4] archive"
rm -f "$lib"
ar rcs "$lib" "${objs[@]}"

echo "[3/4] verify per-TU owned metadata is strong (nm)"
# Every Foundation .jeti implements exactly the class it is named after, so the
# basename IS the owned class. If any of these is weak, ownership did not
# apply and a client TU's stub could win the link — fail loudly instead of
# shipping a landmine.
for gm in include/Foundation/*.jeti; do
    name="$(basename "$gm" .jeti)"
    [[ "$name" == "Foundation" || "$name" == "NPTask" ]] && continue   # no vtable to verify (see [1/4])
    check_strong "$outdir/$name.o" "$name"
done

echo "[4/4] template manifest (so clients can instantiate new specializations)"
# An archive carries only the specializations its own TUs demanded at build
# time. Without the template sources, a client that spells a NEW one
# (`NPArray<MyType *>`) gets an empty weak stub from codegen: it links, and
# jumps to NULL on the first dispatch. The manifest names the sources, so
# `jetic` can compile just the demanded specialization and link it in beside
# the archive (see TemplateManifest in crates/jetic/src/main.rs).
tmpl_lines=()
tmpl_names=()
for hje in include/Foundation/*.hje; do
    cls="$(sed -n 's/^@interface \([A-Za-z_][A-Za-z0-9_]*\)<.*/\1/p' "$hje" | head -1)"
    [[ -z "$cls" ]] && continue
    impl="include/Foundation/$cls.jeti"
    [[ -f "$impl" ]] || continue   # NPTask<T> is a runtime handle, not a class
    # Only the text inside the FIRST `<>`: `@interface NPSet<T> : NPObject`
    # must yield `T`, not `T> : NPObject`.
    params="$(sed -n "s/^@interface ${cls}<\([^>]*\)>.*/\1/p" "$hje" | head -1 | tr ',' ' ')"
    tmpl_lines+=("template $cls $params Foundation/$cls.jeti")
    tmpl_names+=("$cls")
done

# `root` is written relative to the manifest so an installed tree stays
# relocatable; without python3 the absolute include dir is still correct.
include_abs="$(cd include && pwd)"
outdir_abs="$(cd "$outdir" && pwd)"
if command -v python3 >/dev/null 2>&1; then
    root_rel="$(python3 -c 'import os,sys; print(os.path.relpath(sys.argv[1], sys.argv[2]))' "$include_abs" "$outdir_abs")"
else
    root_rel="$include_abs"
fi

# Metadata the archive already defines: a demand matching one of these needs no
# instantiation (instantiating it again would duplicate the library's body).
defined_flats=()
while IFS= read -r flat; do
    [[ -n "$flat" ]] && defined_flats+=("$flat")
done < <(nm -g "$lib" 2>/dev/null | awk '{print $NF}' \
    | grep 'JETI_VTABLE_\$_' | sed 's/.*JETI_VTABLE_\$_//' | sort -u)

manifest="$outdir/libjetifoundation.templates"
{
    echo "# jeti template manifest v1 — generated by tools/build-foundation-lib.sh"
    echo "# A precompiled archive cannot invent a specialization; these entries let"
    echo "# a client's jetic compile the demanded one from the shipped template."
    echo "root $root_rel"
    echo "surface Foundation/Foundation.hje"
    for line in "${tmpl_lines[@]+"${tmpl_lines[@]}"}"; do echo "$line"; done
    # Only specialization-shaped flats are interesting here: a bare class name
    # can never be a demanded specialization's flat.
    for f in "${defined_flats[@]+"${defined_flats[@]}"}"; do
        for c in "${tmpl_names[@]+"${tmpl_names[@]}"}"; do
            if [[ "$f" == "$c"_* ]]; then echo "defined $f"; break; fi
        done
    done
} > "$manifest"
echo "  $manifest ($((${#tmpl_lines[@]})) templates)"

echo "done: $lib ($(du -h "$lib" | cut -f1))"
