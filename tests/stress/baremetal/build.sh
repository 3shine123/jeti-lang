#!/usr/bin/env bash
# tests/stress/baremetal/build.sh
# Full-syntax freestanding stress: transpile with -ffreestanding, compile on the
# host per golden/25 methodology (-include jeti/runtime.h → libc setjmp),
# link runtime_freestanding.c + helpers.c + real ARM64 asm_ext.s.
set -euo pipefail
cd "$(dirname "$0")"

JETIC=../../../target/debug/jetic
BUILD=build
mkdir -p "$BUILD"

echo "== transpile (-ffreestanding) =="
"$JETIC" -rewrite-jeti -ffreestanding -o "$BUILD/baremetal_test.c" baremetal_test.jeti

# Feature-matrix companion: every language feature without Foundation/libc.
# See FREESTANDING.md for the verdict table.
echo "== transpile (freestanding_features) =="
"$JETIC" -rewrite-jeti -ffreestanding -o "$BUILD/freestanding_features.c" freestanding_features.jeti

echo "== compile generated C (golden/25 host methodology) =="
# The generated file has `#define __JETI_FREESTANDING 1` + #include
# <jeti/runtime.h>: runtime.h is DUAL-MODE (no separate bare-metal header).
# On a real bare-metal target (soma-kernel i386) that selects the
# freestanding branch. On THIS host we deliberately pre-load the HOSTED
# branch via -include (it wins the include guard): __builtin_setjmp/longjmp
# is NOT supported on arm64-apple-darwin, and the exception globals must
# match runtime_freestanding.c's compilation mode (both hosted -> __thread).
clang -I../../../include -include jeti/runtime.h \
    -D_FORTIFY_SOURCE=0 -Wno-unused-variable \
    -c "$BUILD/baremetal_test.c" -o "$BUILD/bm.o"

echo "== compile helpers (host console stubs) + bare-metal runtime =="
# Same mode as the generated C above (hosted) so the exception globals
# (__thread) and setjmp semantics match across TUs.
clang -I../../../include -D_FORTIFY_SOURCE=0 \
    -c ../../../include/jeti/runtime_freestanding.c -o "$BUILD/bare_rt.o"
clang -D_FORTIFY_SOURCE=0 -c helpers.c -o "$BUILD/helpers.o"

echo "== assemble asm_ext.s (ARM64) =="
clang -c asm_ext.s -o "$BUILD/asm.o"

echo "== link =="
clang "$BUILD/bm.o" "$BUILD/helpers.o" "$BUILD/bare_rt.o" "$BUILD/asm.o" \
    -o "$BUILD/baremetal_test"

echo "== run =="
"$BUILD/baremetal_test"

echo "== compile freestanding_features (feature matrix, no asm) =="
clang -I../../../include -include jeti/runtime.h \
    -D_FORTIFY_SOURCE=0 -Wno-unused-variable \
    -c "$BUILD/freestanding_features.c" -o "$BUILD/fsf.o"
clang "$BUILD/fsf.o" "$BUILD/helpers.o" "$BUILD/bare_rt.o" \
    -o "$BUILD/freestanding_features"
"$BUILD/freestanding_features"
echo "exit=$?"
