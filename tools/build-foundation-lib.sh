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

# Optimisation flags for the generated C. `-O3` is the default; the archive is
# consumed by other builds, so the semantics- and portability-changing flags
# stay opt-in:
#
#   FOUNDATION_CFLAGS="-O3 -ffast-math -mcpu=native" ./tools/build-foundation-lib.sh
#
# Two measured caveats before you reach for the aggressive set:
#   * `-ffast-math` drops IEEE semantics — clang folds `NaN == NaN` to TRUE,
#     so NaN-guarded logic in NPNumber/NPValue/NPJSONSerialization/NPDate can
#     silently take the wrong branch (verified with a probe on this toolchain).
#   * `-mcpu=native` bakes this machine's CPU features into the archive, so the
#     `.a` may SIGILL on a different CPU. It is accepted by Apple clang here.
# LTO is deliberately absent: the archive is linked by clients that jetic
# compiles without it, so it would buy nothing.
cflags="${FOUNDATION_CFLAGS:--O3}"

# Mach-O prepends `_` to every C symbol; ELF uses the name as written.
sym_prefix=""
case "$(uname -s)" in
    Darwin) sym_prefix="_" ;;
esac

# Weakness is platform-specific in nm output:
#   * Mach-O (Darwin): plain nm shows a weak definition as 'D'/'T' anyway, so
#     `nm -m` and its "weak external" marker are required.
#   * ELF (Linux): weak symbols carry 'W'/'V'/'w'/'v'.
#
# Symbol matching is EXACT, against nm's last field. A substring match
# (`grep -F " $sym"`) is wrong here: `_JETI_VTABLE_$_NPNotification` is a
# prefix of `_JETI_VTABLE_$_NPNotificationCenter`, so every TU's weak stub for
# the longer name made the owner's own strong symbol read as weak — a false
# R2 failure the moment the library gained its first class-name prefix
# collision. nm puts the symbol last in both plain and `-m` output.
nm_lines_for() {
    local obj="$1" sym="$2"
    nm "$obj" 2>/dev/null | awk -v s="$sym" '$NF == s'
}

nm_is_weak() {
    local obj="$1" sym="$2"
    case "$(uname -s)" in
        Darwin)
            nm -m "$obj" 2>/dev/null | awk -v s="$sym" '$NF == s' | grep -q "weak external"
            ;;
        *)
            nm "$obj" 2>/dev/null | awk -v s="$sym" '$NF == s' \
                | grep -qE '^[0-9a-fA-F]+[[:space:]]+[WwVv][[:space:]]'
            ;;
    esac
}

check_strong() {
    local obj="$1" cls="$2"
    for member in "JETI_VTABLE_\$_${cls}" "JETI_META_VTABLE_\$_${cls}_inst" "JETI_GETCLASS_\$_${cls}"; do
        local sym="${sym_prefix}${member}"
        if [ -z "$(nm_lines_for "$obj" "$sym")" ]; then
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

echo "[1/5] transpile + compile each Foundation TU (wrapper = decl surface + implementation)"
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
rm -f "$outdir"/*.o   # stale objects from an earlier run must not linger into [3/5]
# Pinned slot layout shipped beside the archive (`libjetifoundation.slots`).
# A client that imports its OWN headers still sees the same selector→slot
# assignment for every Foundation selector, so adding a class cannot shift a
# Foundation class's indices and trip the R3 layout check at startup. Rebuilt
# from scratch each run: the manifest is append-only, and a stale one would
# freeze slot order from a header set that no longer exists.
slots="$outdir/libjetifoundation.slots"
rm -f "$slots"
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
    # MRC by design (user decision): Foundation is the layer that implements
    # the memory primitives, so it manages retain/release explicitly and is
    # compiled with ARC insertion disabled — never at the mercy of the
    # checker's ARC analysis for its own correctness.
    "$jetic" -rewrite-jeti "$wrap" -o "$c" -I include -I include/Foundation -fno-jeti-arc --slots "$slots"
    # -fblocks：Foundation 头里有 block 形参（enumerateUsingBlock: 等），原型
    # 展开即含 `(^`。Apple clang 默认开；Linux/上游 clang 必须显式给，否则
    # 归档里的原型就编不过（链接侧的 BlocksRuntime 由 jetic 在客户端补）。
    clang -c -w -fblocks $cflags "$c" -o "$o" -I include
    objs+=("$o")
    echo "  $name"
done

echo "[2/5] archive"
rm -f "$lib"
ar rcs "$lib" "${objs[@]}"

echo "[3/5] verify per-TU owned metadata is strong (nm)"
# Every Foundation .jeti implements exactly the class it is named after, so the
# basename IS the owned class. If any of these is weak, ownership did not
# apply and a client TU's stub could win the link — fail loudly instead of
# shipping a landmine.
for gm in include/Foundation/*.jeti; do
    name="$(basename "$gm" .jeti)"
    [[ "$name" == "Foundation" || "$name" == "NPTask" ]] && continue   # no vtable to verify (see [1/5])
    check_strong "$outdir/$name.o" "$name"
done

echo "[4/5] template manifest (so clients can instantiate new specializations)"
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

echo "[5/5] shared library (dynamic linking: .dylib / .so / .dll)"
# The shared library bundles the RUNTIME as well. That is what lets a client
# link ~1/4 the size: with the runtime and every class inside the library, the
# client keeps only its own translation unit. It also means a client that links
# this library must NOT compile the bundled runtime.c — two copies of the
# exception state / autorelease-pool head / jeti_argc would silently diverge.
# jetic handles that side: it skips runtime.c exactly when it links the shared
# Foundation (see `find_libjetifoundation` / the link step in main.rs).
#
# Failure here is NOT fatal: the static archive is already complete and is the
# fallback, so a platform without a usable shared-lib or Blocks runtime still
# gets a working Foundation.
#
# OFF BY DEFAULT, and jetic does not link the result. Linking a client against a
# shared Foundation was measured to break KVC-driven code (NPPredicate filtering
# returns 0 matches, even for pure Foundation objects): a declaration-only
# client emits the non-owned classes' NPClass metadata as zeroed tentative
# definitions with a NULL `.kvc_entries`, which statically lose the weak merge to
# the archive's strong tables but dynamically are what the client's own objects
# point at. Enabling this needs a metadata-interposition design first — see the
# KNOWN ISSUE note on `foundation_shared_name` in crates/jetic/src/main.rs.
# Set FOUNDATION_SHARED=1 to build the artifact for that work.
if [[ "${FOUNDATION_SHARED:-0}" != "1" ]]; then
    # Remove any shared library left by an earlier FOUNDATION_SHARED=1 run: a
    # linker resolves `-ljetifoundation` in favour of the shared library when
    # one sits in the search directory, so a stale artifact would silently drag
    # clients onto the prototyped (KVC-broken) path.
    rm -f "$outdir"/libjetifoundation.dylib "$outdir"/libjetifoundation.so \
          "$outdir"/libjetifoundation.dll "$outdir"/runtime.o
    echo "  (skipped: set FOUNDATION_SHARED=1 to build — see the KNOWN ISSUE in main.rs)"
fi
shared_lib=""
shared_flags=()
case "$(uname -s)" in
    Darwin)  shared_lib="$outdir/libjetifoundation.dylib"
             shared_flags=(-dynamiclib -install_name "@rpath/libjetifoundation.dylib") ;;
    Linux)   shared_lib="$outdir/libjetifoundation.so"
             shared_flags=(-shared -Wl,-soname,libjetifoundation.so) ;;
    MINGW*|MSYS*|CYGWIN*|Windows*) shared_lib="$outdir/libjetifoundation.dll"
             shared_flags=(-shared) ;;
    *)       shared_lib="" ;;
esac

if [[ "${FOUNDATION_SHARED:-0}" == "1" && -n "$shared_lib" ]]; then
    runtime_o="$outdir/runtime.o"
    clang -c -w $cflags include/jeti/runtime.c -o "$runtime_o" -I include
    rm -f "$shared_lib"
    if clang "${shared_flags[@]}" -o "$shared_lib" "${objs[@]}" "$runtime_o" 2>/dev/null; then
        echo "  $shared_lib ($(du -h "$shared_lib" | cut -f1))"
    # Blocks need an explicit runtime on ELF (Darwin gets it from libSystem).
    elif [[ "$(uname -s)" == "Linux" ]] \
        && clang "${shared_flags[@]}" -o "$shared_lib" "${objs[@]}" "$runtime_o" -lBlocksRuntime 2>/dev/null; then
        echo "  $shared_lib (+libBlocksRuntime, $(du -h "$shared_lib" | cut -f1))"
    else
        rm -f "$shared_lib"
        echo "  (skipped: shared-library link failed — clients fall back to the static archive)" >&2
    fi
fi

echo "done: $lib ($(du -h "$lib" | cut -f1))"
if [[ -f "$shared_lib" ]]; then
    echo "      $shared_lib ($(du -h "$shared_lib" | cut -f1)) — preferred by jetic when present"
fi
