#!/usr/bin/env bash
# run_strong_metadata_test.sh — protection tests for class-metadata linkage.
#
# Guards the precompiled-library (P3) contract from docs/plans/stable_slots_plan.md §8
# on three levels, because "the program printed the right thing" alone does not
# prove the metadata linkage is right:
#
#   1. OBJECT FILE — the library's class metadata is actually STRONG
#      (nm), not merely weak-merged by luck. Reuses tools/build-foundation-lib.sh,
#      which also enforces this.
#   2. LINK + RUN  — a client TU that only sees declarations (weak stubs) links
#      against the strong library and runs correctly.
#   3. LOUD FAILURE — the deleted -fstrong-metadata spelling is rejected with
#      "Unknown argument": plan A removed the manual switch, so a legacy
#      script still carrying it must fail loudly instead of half-working.
#      (Duplicate-symbol loudness for two owners lives in check 4; against
#      the per-TU *archive* a client re-implementation is standard C override
#      semantics — the library member stays dormant unless referenced.)
#   4. R2 OWNERSHIP — two TUs that both `@implementation` the same class are
#      both owners and must be a duplicate symbol (no flag involved).
#   5. AUTO-STRONG — the flag is NOT required for the standalone-TU workflow:
#      a main file (.hje or .jeti) holding an `@implementation` emits STRONG
#      metadata for its owned class with no flag at all (ownership is derived
#      from the main file). Guards the rule from regressing into
#      "must remember -fstrong-metadata by hand".
#   6. CLIENT STAYS WEAK — a declaration-only `.hje` (no @implementation
#      reaching the TU) must NOT go strong: its stubs are the client side of
#      rule R2 and must lose to the owner's table.
#
# Out-of-band (like tests/multi_tu): not part of test_all.py.
set -uo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
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

work="$(mktemp -d "${TMPDIR:-/tmp}/jeti_strong_meta.XXXXXX")"
trap 'rm -rf "$work"' EXIT

pass=0; fail=0
ok()   { echo "PASS  $1"; pass=$((pass+1)); }
bad()  { echo "FAIL  $1"; fail=$((fail+1)); }

lib="$work/lib"
client="$work/client.jeti"
cat > "$client" <<'EOF'
#import <Foundation/Foundation.hje>
#include <stdio.h>
int main() {
    @autoreleasepool {
        NPString *s = [NPString stringWithUTF8String:"hello"];
        printf("len=%zu s=%s\n", [s length], [s UTF8String]);
    }
    return 0;
}
EOF

# ── 1. library metadata is strong (check 1 lives inside the build script) ──
if JETIC="$jetic" ./tools/build-foundation-lib.sh "$lib" > "$work/lib.log" 2>&1; then
    ok "1. library built with strong metadata (nm verified by the build script)"
else
    bad "1. library build / strong-metadata verification"
    sed 's/^/      /' "$work/lib.log" | tail -20
fi

# ── 2. weak client + strong library: link and run ──
if "$jetic" "$client" -I include -L "$lib" -ljetifoundation -o "$work/app" > "$work/app.log" 2>&1; then
    out="$("$work/app" 2>&1)"; rc=$?
    if [[ $rc -eq 0 && "$out" == *"len=5 s=hello"* ]]; then
        ok "2. declaration-only client links against the strong library and runs"
    else
        bad "2. client ran but produced the wrong result (rc=$rc: $out)"
    fi
else
    bad "2. client failed to link against the strong library"
    sed 's/^/      /' "$work/app.log" | tail -20
fi

# ── 3. LOUD FAILURE: the old manual switch is gone ──
# Plan A removed -fstrong-metadata: ownership is derived by construction
# (R2), so there must be no manual strong-metadata path left. A script still
# carrying the old spelling must be rejected, not silently accepted.
cat > "$work/flag_probe.jeti" <<'EOF'
#import <Foundation/Foundation.hje>
int main() { return 0; }
EOF
# Flag AFTER the input hits the option loop → "Unknown argument"; flag BEFORE
# the input falls into the positional-input slot → "cannot read" — both orders
# must fail loudly, and the post-input one must name the flag.
if "$jetic" "$work/flag_probe.jeti" -fstrong-metadata -I include -o "$work/app_dup" > "$work/dup.log" 2>&1; then
    bad "3. -fstrong-metadata was accepted — the deleted flag still parses"
else
    if grep -q "Unknown argument: -fstrong-metadata" "$work/dup.log" \
            && ! "$jetic" -fstrong-metadata "$work/flag_probe.jeti" -I include -o "$work/app_dup2" > "$work/dup2.log" 2>&1; then
        ok "3. deleted -fstrong-metadata is rejected loudly (Unknown argument; both arg orders fail)"
    else
        bad "3. old flag failed, but not with the expected diagnostics"
        sed 's/^/      /' "$work/dup.log" "$work/dup2.log" | tail -8
    fi
fi

# ── 4. rule R2: two owners of the same class must not merge silently ──
# No flag involved: ownership is derived from the main file, so two TUs that
# both `@implementation` Widget are both owners and the link must reject them.
cat > "$work/model.hje" <<'EOF'
@interface Widget
- (void)show;
@end
EOF
cat > "$work/lib_a.jeti" <<'EOF'
#import "model.hje"
@implementation Widget
- (void)show { }
@end
EOF
cat > "$work/lib_b.jeti" <<'EOF'
#import "model.hje"
@implementation Widget
- (void)show { }
@end
EOF
cat > "$work/driver.jeti" <<'EOF'
#import "model.hje"
int main() { Widget *w = 0; if (w) { [w show]; } return 0; }
EOF
if "$jetic" -rewrite-jeti "$work/lib_a.jeti" -I "$work" -I include -o "$work/a.c" > "$work/a.log" 2>&1 \
   && "$jetic" -rewrite-jeti "$work/lib_b.jeti" -I "$work" -I include -o "$work/b.c" > "$work/b.log" 2>&1 \
   && "$jetic" -rewrite-jeti "$work/driver.jeti" -I "$work" -I include -o "$work/driver.c" >> "$work/a.log" 2>&1; then
    if clang -std=c99 -w "$work/a.c" "$work/b.c" "$work/driver.c" include/jeti/runtime.c \
            -I include -o "$work/dup_impl" > "$work/dup_impl.log" 2>&1; then
        bad "4. two TUs owning the same class linked without error (R2 regression)"
    elif grep -q "duplicate symbol" "$work/dup_impl.log"; then
        ok "4. two TUs owning the same class are rejected with 'duplicate symbol' (R2)"
    else
        bad "4. link failed, but not with a duplicate-symbol diagnostic"
        sed 's/^/      /' "$work/dup_impl.log" | tail -12
    fi
else
    bad "4. could not transpile the two-owner case"
    sed 's/^/      /' "$work/a.log" "$work/b.log" | tail -12
fi

# ── 5/6 helpers: read symbol strength straight from the object file ──
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
sym_present() { nm "$1" 2>/dev/null | grep -qF -- " $2"; }
check_strong() {
    local obj="$1" sym="$2" label="$3"
    if ! sym_present "$obj" "$sym"; then
        bad "$label ('$sym' absent)"
    elif nm_is_weak "$obj" "$sym"; then
        bad "$label ('$sym' is weak — automatic strong metadata regressed)"
    else
        ok "$label"
    fi
}
check_weak() {
    local obj="$1" sym="$2" label="$3"
    if ! sym_present "$obj" "$sym"; then
        bad "$label ('$sym' absent)"
    elif nm_is_weak "$obj" "$sym"; then
        ok "$label"
    else
        bad "$label ('$sym' is strong — a declaration-only TU must stay weak)"
    fi
}

# ── 5. AUTO-STRONG: a main file holding @implementation needs no flag ──
# Both spellings of a standalone TU — a self-contained .hje and a .jeti — must
# emit STRONG metadata for the class they own, with no -fstrong-metadata.
cat > "$work/own_iface.hje" <<'EOF'
@interface OwnGm : NPObject
- (int)ping;
@end
EOF
cat > "$work/own.hje" <<'EOF'
#import <Foundation/Foundation.hje>
@interface OwnGh : NPObject
- (int)ping;
@end
@implementation OwnGh : NPObject
- (int)ping { return 42; }
@end
EOF
cat > "$work/own.jeti" <<'EOF'
#import <Foundation/Foundation.hje>
#import "own_iface.hje"
@implementation OwnGm : NPObject
- (int)ping { return 7; }
@end
EOF
gh_ok=1 gm_ok=1
"$jetic" -rewrite-jeti "$work/own.hje" -I include -o "$work/owngh.c" > "$work/owngh.log" 2>&1 \
    && clang -c -w "$work/owngh.c" -o "$work/owngh.o" -I include || gh_ok=0
"$jetic" -rewrite-jeti "$work/own.jeti" -I "$work" -I include -o "$work/owngm.c" > "$work/owngm.log" 2>&1 \
    && clang -c -w "$work/owngm.c" -o "$work/owngm.o" -I include || gm_ok=0
if [[ $gh_ok -eq 1 && $gm_ok -eq 1 ]]; then
    check_strong "$work/owngh.o" "_JETI_VTABLE_\$_OwnGh" "5a. standalone .hje with @implementation auto-strong (no flag)"
    check_strong "$work/owngh.o" "_JETI_META_VTABLE_\$_OwnGh_inst" "5b. ... and its meta vtable too"
    check_strong "$work/owngm.o" "_JETI_VTABLE_\$_OwnGm" "5c. standalone .jeti with @implementation auto-strong (no flag)"
    check_strong "$work/owngm.o" "_JETI_META_VTABLE_\$_OwnGm_inst" "5d. ... and its meta vtable too"
else
    bad "5. could not transpile/compile the standalone-TU probes"
    sed 's/^/      /' "$work/owngh.log" "$work/owngm.log" | tail -12
fi

# ── 6. CLIENT STAYS WEAK: a declaration-only .hje must not go strong ──
# Its NULL-filled stubs are the client side of rule R2; they must lose to the
# owner's table, so they have to stay weak even without any flag.
cat > "$work/client_decl.hje" <<'EOF'
#import <Foundation/Foundation.hje>
@interface ClientGadget : NPObject
- (int)ping;
@end
EOF
if "$jetic" -rewrite-jeti "$work/client_decl.hje" -I include -o "$work/client_decl.c" > "$work/cd.log" 2>&1 \
        && clang -c -w "$work/client_decl.c" -o "$work/client_decl.o" -I include; then
    check_weak "$work/client_decl.o" "_JETI_VTABLE_\$_ClientGadget" "6. declaration-only .hje stub stays weak (no flag)"
else
    bad "6. could not transpile/compile the declaration-only probe"
    sed 's/^/      /' "$work/cd.log" | tail -12
fi

# ── 7. §9 STRUCT DEDUP: a main file owning the root classes compiles clean ──
# Section 4 emits guarded fallbacks for jeti_root/NPObject; the class-layout
# section used to re-emit the same bodies unguarded in the same file — a hard
# C redefinition error. The dedup records Section 4's bodies, so each root
# struct is defined exactly once and the Foundation root TU compiles alone.
dedup_ok=1
if "$jetic" -rewrite-jeti include/Foundation/NPObject.jeti -I include -o "$work/npo.c" > "$work/npo.log" 2>&1; then
    root_defs=$(grep -c 'struct jeti_root {' "$work/npo.c")
    obj_defs=$(grep -c 'struct NPObject {' "$work/npo.c")
    if [[ "$root_defs" == "1" && "$obj_defs" == "1" ]] \
            && clang -c -w "$work/npo.c" -o "$work/npo.o" -I include 2>>"$work/npo.log"; then
        ok "7. root-struct dedup: NPObject.jeti as a main file compiles clean (one definition each)"
    else
        dedup_ok=0
    fi
else
    dedup_ok=0
fi
if [[ $dedup_ok -eq 0 ]]; then
    bad "7. root-struct dedup regressed (same-file double definition or compile failure)"
    sed 's/^/      /' "$work/npo.log" | tail -12
fi

echo "----"
echo "strong-metadata: $pass passed, $fail failed"
exit $((fail > 0))
