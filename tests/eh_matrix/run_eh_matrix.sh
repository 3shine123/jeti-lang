#!/usr/bin/env bash
# tests/eh_matrix/run_eh_matrix.sh — EH backend acceptance matrix.
#
# WHY THIS EXISTS
# ---------------
# `-eh checked` replaces the sjlj (setjmp/longjmp) exception backend with an
# explicit flag + guard lowering. Flipping it to the DEFAULT is gated on
# evidence, not on "it passes the goldens": every scenario below must be run in
# all three modes and produce a recorded verdict.
#
#   default   no -eh flag        (the shipped default; sjlj today)
#   checked   -eh checked        (candidate default)
#   legacy    -eh legacy         (explicit sjlj alias — must stay usable as a
#                                 full rollback for the flipped default)
#
# Verdicts per (scenario, mode):
#   PASS         exit 0 and, when a reference .out exists, stdout matches it
#   FAIL(rc=N)   the program exited non-zero
#   FAIL(out)    stdout differs from the reference .out
#   FAIL(cc)     transpile/compile/link failed (special scenarios)
#   DIFF         three modes disagree on stdout (semantic drift between
#                backends — the matrix fails, since "checked is the default"
#                must not change observable behavior)
#
# Usage:
#   ./run_eh_matrix.sh                # full matrix
#   JETIC=target/debug/jetic ./run_eh_matrix.sh
set -u
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$ROOT"

JETIC="${JETIC:-}"
if [[ -z "$JETIC" ]]; then
    for cand in target/release/jetic target/debug/jetic; do
        if [[ -x "$cand" ]]; then JETIC="$cand"; break; fi
    done
fi
if [[ ! -x "$JETIC" ]]; then
    echo "error: jetic binary not found (build it, or set JETIC=)" >&2
    exit 2
fi
JETIC_ABS="$(cd "$(dirname "$JETIC")" && pwd)/$(basename "$JETIC")"

WORK="${TMPDIR:-/tmp}/jeti_eh_matrix.$$"
mkdir -p "$WORK"
trap 'rm -rf "$WORK"' EXIT

ROWS="$WORK/rows.txt"      # scene|mode|status
: > "$ROWS"

MODES=(default checked legacy)

eh_args_for() {
    case "$1" in
        checked) printf -- '-eh checked' ;;
        legacy)  printf -- '-eh legacy' ;;
        *)       printf '' ;;
    esac
}

# eh_argv <mode> -> fills global EH_ARGV array (bash-safe, no word splitting)
eh_argv() {
    EH_ARGV=()
    case "$1" in
        checked) EH_ARGV=(-eh checked) ;;
        legacy)  EH_ARGV=(-eh legacy) ;;
    esac
}

record() { printf '%s|%s|%s\n' "$1" "$2" "$3" >> "$ROWS"; }

# Compare stdout to a reference .out, ignoring trailing-blank-line differences:
# several golden .out files were recorded without a final newline, which is not
# an observable behavior difference.
same_output() {
    diff -q <(norm_tail "$1") <(norm_tail "$2") >/dev/null 2>&1
}

norm_tail() {
    awk '{L[n++]=$0} END{while(n>0 && L[n-1]=="") n--; for(i=0;i<n;i++) print L[i]}' "$1"
}

echo "=== EH acceptance matrix ==="
echo "jetic : $JETIC_ABS"
echo "clang : $(clang --version | head -1)"
echo

# ─────────────────────────────────────────────────────────────────────────────
# Generic scenario: run a .jeti in all three modes, compare stdout to the
# reference .out when one is supplied.
#   run_scene <scene> <file> <ref-or-empty> [extra jetic args...]
# ─────────────────────────────────────────────────────────────────────────────
run_scene() {
    local scene="$1" file="$2" ref="$3"; shift 3
    local extra=("$@")
    local logs=()
    for m in "${MODES[@]}"; do
        eh_argv "$m"
        local d="$WORK/$scene.$m"
        local log="$d.stdout" errf="$d.stderr" rcfile="$d.rc" bin="$d.bin"
        "$JETIC_ABS" run -o "$bin" ${EH_ARGV[@]+"${EH_ARGV[@]}"} \
            ${extra[@]+"${extra[@]}"} "$file" >"$log" 2>"$errf"
        local rc=$?
        local used_mrc=0
        # ARC->MRC retry: the exact fallback test_all.py applies to goldens
        # that still carry explicit retain/release.
        if [[ $rc -ne 0 ]] && grep -q "not allowed in ARC mode" "$errf"; then
            "$JETIC_ABS" run -o "$bin" -fno-jeti-arc ${EH_ARGV[@]+"${EH_ARGV[@]}"} \
                ${extra[@]+"${extra[@]}"} "$file" >"$log" 2>"$errf"
            rc=$?; used_mrc=1
        fi
        echo "$rc" > "$rcfile"
        local status="PASS"
        [[ $used_mrc -eq 1 ]] && status="PASS(mrc)"
        if [[ $rc -ne 0 ]]; then
            status="FAIL(rc=$rc)"
        elif [[ -n "$ref" ]] && ! same_output "$ref" "$log"; then
            status="FAIL(out)"
        fi
        record "$scene" "$m" "$status"
    done
    report_scene "$scene"
}

# Gates:
#   1. DEFAULT PARITY — the bare invocation (no -eh) must be byte-identical to
#      an explicit `-eh checked` (rc + stdout + stderr). If it is not, the
#      flipped default is not the vetted backend. (Before the flip this gate
#      compared default against `-eh legacy`; now `default` IS checked, and the
#      "legacy really is sjlj" half is pinned by default_backend_gate.)
#   2. CROSS-MODE DRIFT — when all three modes succeed, their stdout must
#      agree. `-eh legacy` is expected to diverge only on the documented sjlj
#      limitation scenarios, which are recorded separately.
report_scene() {
    local scene="$1"
    if ! diff -q "$WORK/$scene.default.rc" "$WORK/$scene.checked.rc" >/dev/null 2>&1 \
    || ! diff -q "$WORK/$scene.default.stdout" "$WORK/$scene.checked.stdout" >/dev/null 2>&1 \
    || ! diff -q "$WORK/$scene.default.stderr" "$WORK/$scene.checked.stderr" >/dev/null 2>&1; then
        grep -v "^${scene}|" "$ROWS" > "$ROWS.tmp" && mv "$ROWS.tmp" "$ROWS"
        for m in "${MODES[@]}"; do record "$scene" "$m" "JETIRITY"; done
        echo "  ! $scene — default and explicit -eh checked DISAGREE (flip is not behavior-preserving)"
        return
    fi
    local all_ran=1
    for m in "${MODES[@]}"; do
        [[ "$(cat "$WORK/$scene.$m.rc" 2>/dev/null)" == "0" ]] || all_ran=0
    done
    [[ $all_ran -eq 1 ]] || return 0
    if ! diff -q "$WORK/$scene.default.stdout" "$WORK/$scene.legacy.stdout" >/dev/null 2>&1; then
        grep -v "^${scene}|" "$ROWS" > "$ROWS.tmp" && mv "$ROWS.tmp" "$ROWS"
        for m in "${MODES[@]}"; do record "$scene" "$m" "DIFF"; done
        echo "  ! $scene — legacy (sjlj) and the default disagree on stdout"
    fi
}

# ─────────────────────────────────────────────────────────────────────────────
# Scenario table (generic entries). name | file | reference .out ('' = none) |
# extra args
# ─────────────────────────────────────────────────────────────────────────────
GOLD="tests/golden"
run_scene "01_plain_jeti"      "$GOLD/01_basics/hello_world.jeti"                    "$GOLD/01_basics/hello_world.out"
run_scene "02_try_catch"       "$GOLD/15_exceptions/try_catch.jeti"                  "$GOLD/15_exceptions/try_catch.out"
run_scene "03_nested_try"      "tests/eh_diff/03_nested_finally.jeti"                ""
run_scene "04_finally_order"   "tests/eh_matrix/samples/finally_order.jeti"          ""
run_scene "05_rethrow"         "tests/eh_diff/04_rethrow.jeti"                       ""
run_scene "06_cross_frame"     "tests/eh_diff/01_cross_frame_release.jeti"           ""
run_scene "07_arc"             "$GOLD/04_arc/retain_release.jeti"                    "$GOLD/04_arc/retain_release.out"
# 08_mrc has no .out reference on purpose: retain_release.out is the ARC
# transcript (it ends with a `dealloc` line MRC never prints, since MRC does
# not release the object). The verdict here is "runs, and all three modes
# agree", which report_scene enforces.
run_scene "08_mrc"             "$GOLD/04_arc/retain_release.jeti"                    "" -fno-jeti-arc
run_scene "09_autoreleasepool" "$GOLD/05_autoreleasepool/nested_pool.jeti"           "$GOLD/05_autoreleasepool/nested_pool.out"
run_scene "11_foundation_big"  "$GOLD/13_foundation/04_npstring/npstring_test.jeti"  "$GOLD/13_foundation/04_npstring/npstring_test.out"
run_scene "12_c_superset"      "$GOLD/22_c_superset/c_superset.jeti"                 ""
run_scene "15_await_eh"        "tests/eh_matrix/samples/async_eh.jeti"               ""
run_scene "16_block_throw"     "tests/eh_diff/06_block_throw.jeti"                   ""
# 17: the guard-density gate's runtime half — a program that cannot throw must
# run identically in all three modes (the static half, "checked emits zero
# guards", lives in gen_quality.sh).
run_scene "17_no_throw_plain"  "tests/eh_matrix/samples/no_throw_plain.jeti"         ""
# 18: full-syntax sweep (C superset + Foundation + inline asm + throws + await)
run_scene "18_full_syntax"     "tests/full_syntax_test.jeti"                         "" -asm tests/full_syntax_test.s

# ─────────────────────────────────────────────────────────────────────────────
# 10. multi-TU: two separately-transpiled translation units linked together.
#     Driven by tests/multi_tu/run_multi_tu.sh, which honours JETI_EH_FLAG.
# ─────────────────────────────────────────────────────────────────────────────
scene_multi_tu() {
    local scene="10_multi_tu"
    for m in "${MODES[@]}"; do
        eh_argv "$m"
        local log="$WORK/${scene}.${m}.log"
        # Every case, not just one: this is both the multi-TU/x-TU-layout gate
        # and a Foundation-inlining (large concatenated TU) gate.
        JETI_EH_FLAG="${EH_ARGV[*]:-}" JETIC="$JETIC_ABS" \
            ./tests/multi_tu/run_multi_tu.sh >"$log" 2>&1
        if grep -qE "^multi-TU: [0-9]+ passed, 0 failed" "$log"; then
            record "$scene" "$m" "PASS"
        else
            record "$scene" "$m" "FAIL(cc)"
        fi
    done
}
scene_multi_tu

# ─────────────────────────────────────────────────────────────────────────────
# 13. -ffreestanding: transpile bare-metal C, compile on the host with the
#     golden/25 methodology, link runtime_freestanding.c + helpers, run.
# ─────────────────────────────────────────────────────────────────────────────
scene_freestanding() {
    local scene="13_freestanding"
    for m in "${MODES[@]}"; do
        eh_argv "$m"
        local d="$WORK/$scene.$m"; mkdir -p "$d"
        local log="$d/build.log"
        {
            "$JETIC_ABS" -rewrite-jeti -ffreestanding ${EH_ARGV[@]+"${EH_ARGV[@]}"} \
                -o "$d/fs.c" tests/golden/25_freestanding/freestanding.jeti || exit 10
            clang -I include -include jeti/runtime.h -D_FORTIFY_SOURCE=0 \
                -Wno-unused-variable -c "$d/fs.c" -o "$d/fs.o" || exit 11
            clang -I include -U__JETI_FREESTANDING -D_FORTIFY_SOURCE=0 \
                -c tests/golden/25_freestanding/helpers.c -o "$d/helpers.o" || exit 12
            clang -I include -U__JETI_FREESTANDING -D_FORTIFY_SOURCE=0 \
                -c include/jeti/runtime_freestanding.c -o "$d/rt.o" || exit 13
            clang "$d/fs.o" "$d/helpers.o" "$d/rt.o" -o "$d/fs" || exit 14
            "$d/fs" || exit 15
        } >"$log" 2>&1
        if [[ $? -eq 0 ]]; then record "$scene" "$m" "PASS"; else record "$scene" "$m" "FAIL(cc)"; fi
    done
}
scene_freestanding

# ─────────────────────────────────────────────────────────────────────────────
# 14. ARM64 bare metal: -ffreestanding + real asm_ext.s + helpers.c, per
#     tests/stress/baremetal/build.sh.
# ─────────────────────────────────────────────────────────────────────────────
scene_baremetal() {
    local scene="14_baremetal_arm64"
    local bm="tests/stress/baremetal"
    for m in "${MODES[@]}"; do
        eh_argv "$m"
        local d="$WORK/$scene.$m"; mkdir -p "$d"
        local log="$d/build.log"
        {
            "$JETIC_ABS" -rewrite-jeti -ffreestanding ${EH_ARGV[@]+"${EH_ARGV[@]}"} \
                -o "$d/bm.c" "$bm/baremetal_test.jeti" || exit 10
            clang -I include -include jeti/runtime.h -D_FORTIFY_SOURCE=0 \
                -Wno-unused-variable -c "$d/bm.c" -o "$d/bm.o" || exit 11
            clang -I include -D_FORTIFY_SOURCE=0 \
                -c include/jeti/runtime_freestanding.c -o "$d/rt.o" || exit 12
            clang -D_FORTIFY_SOURCE=0 -c "$bm/helpers.c" -o "$d/helpers.o" || exit 13
            clang -c "$bm/asm_ext.s" -o "$d/asm.o" || exit 14
            clang "$d/bm.o" "$d/helpers.o" "$d/rt.o" "$d/asm.o" -o "$d/bm" || exit 15
            "$d/bm" || exit 16
        } >"$log" 2>&1
        if [[ $? -eq 0 ]]; then record "$scene" "$m" "PASS"; else record "$scene" "$m" "FAIL(cc)"; fi
    done
}
scene_baremetal

# ─────────────────────────────────────────────────────────────────────────────
# Gate: the DEFAULT invocation (no -eh flag) must actually be the checked
# backend, and `-eh legacy` must actually be sjlj. Fingerprints:
#   checked : __jeti_eh_flag present, setjmp/longjmp absent
#   sjlj    : setjmp/longjmp present, __jeti_eh_flag absent
# ─────────────────────────────────────────────────────────────────────────────
default_backend_gate() {
    local probe="tests/golden/15_exceptions/try_catch.jeti"
    local d="$WORK/default_gate"; mkdir -p "$d"
    "$JETIC_ABS" -rewrite-jeti -fno-jeti-arc -o "$d/default.c" "$probe" >/dev/null 2>&1
    "$JETIC_ABS" -rewrite-jeti -eh legacy -fno-jeti-arc -o "$d/legacy.c" "$probe" >/dev/null 2>&1
    local dflag dsetj lflag lsetj
    dflag=$(grep -c "__jeti_eh_flag" "$d/default.c" || true)
    dsetj=$(grep -cE "setjmp|longjmp" "$d/default.c" || true)
    lflag=$(grep -c "__jeti_eh_flag" "$d/legacy.c" || true)
    lsetj=$(grep -cE "setjmp|longjmp" "$d/legacy.c" || true)
    echo "== default-backend gate =="
    echo "  no -eh flag : flag=$dflag setjmp=$dsetj   (want flag>0 setjmp=0 -> checked)"
    echo "  -eh legacy  : flag=$lflag setjmp=$lsetj   (want flag=0 setjmp>0 -> sjlj)"
    if [[ "$dflag" -gt 0 && "$dsetj" -eq 0 && "$lflag" -eq 0 && "$lsetj" -gt 0 ]]; then
        echo "  GATE OK — default is checked, legacy is sjlj"
        return 0
    fi
    echo "  GATE FAIL — default/legacy backend selection is wrong"
    return 1
}

GATE_RC=0
default_backend_gate || GATE_RC=1
echo

# ─────────────────────────────────────────────────────────────────────────────
# Print the matrix
# ─────────────────────────────────────────────────────────────────────────────
SCENES=$(cut -d'|' -f1 "$ROWS" | awk '!seen[$0]++')

# Scenarios where the sjlj backend is KNOWN to differ from checked (documented
# limitations, not regressions): cross-frame cleanup of an intermediate frame's
# owned locals on a throw. `checked` exists to fix exactly this, so a
# checked-PASS / sjlj-FAIL split here is "checked-only", not a matrix failure.
# Anything else failing is a real failure.
KNOWN_SJLJ_LIMITATION="06_cross_frame"

# NOTE: `default` and `checked` are expected to be identical since the flip —
# that identity is itself the gate (a bare invocation must select checked).
printf '%-22s %-14s %-14s %-14s %s\n' "scenario" "default" "checked" "legacy" "verdict"
printf -- '---------------------------------------------------------------------------\n'
TOTAL_FAIL=0
for s in $SCENES; do
    row=()
    for m in "${MODES[@]}"; do
        v="$(grep "^${s}|${m}|" "$ROWS" | cut -d'|' -f3 | tail -1)"
        row+=("${v:-n/a}")
    done
    verdict="OK"
    for v in "${row[@]}"; do
        case "$v" in
            PASS|PASS\(mrc\)) ;;
            JETIRITY) verdict="DEFAULT!=CHECKED" ;;
            DIFF) verdict="MODE-DRIFT" ;;
            *) [[ "$verdict" == OK || "$verdict" == FAIL ]] && verdict="FAIL" ;;
        esac
    done
    if [[ "$verdict" == FAIL ]] && [[ " $KNOWN_SJLJ_LIMITATION " == *" $s "* ]] \
       && [[ "${row[1]}" == PASS* ]]; then
        verdict="checked-only(sjlj limit)"
    fi
    case "$verdict" in
        OK|checked-only*) ;;
        *) TOTAL_FAIL=$((TOTAL_FAIL+1)) ;;
    esac
    printf '%-22s %-14s %-14s %-14s %s\n' "$s" "${row[0]}" "${row[1]}" "${row[2]}" "$verdict"
done
echo "---------------------------------------------------------------------------"
echo "scenes: $(echo "$SCENES" | wc -l | tr -d ' ')   failing scenes: $TOTAL_FAIL"
[[ $TOTAL_FAIL -eq 0 && $GATE_RC -eq 0 ]]
