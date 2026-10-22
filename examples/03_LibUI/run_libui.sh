#!/bin/bash
# examples/03_LibUI/run_libui.sh — transpile, compile, link, run the libui-ng demo.
#
# Pure Jeti: the only sources are .jeti/.hje files. The demo inlines the wrapper
# (include/LibUI.jeti → one .c file), which is compiled and linked with the
# Jeti runtime — no hand-written .c/.m files anywhere.
#
# Requires: libui-ng built with meson (set LIBUI_DIR to your checkout)
#           jetic (built at ../../target/debug/jetic or ../../target/release/jetic)
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
JETILANG="$(cd "$SCRIPT_DIR/../.." && pwd)"

if [ -n "${JETIC:-}" ]; then
    JETIC="$JETIC"
elif [ -x "$JETILANG/target/debug/jetic" ]; then
    JETIC="$JETILANG/target/debug/jetic"
else
    JETIC="$JETILANG/target/release/jetic"
fi

if [ -n "${LIBUI_DIR:-}" ]; then
    LIBUI="$LIBUI_DIR"
elif [ -d "$JETILANG/../libui-ng" ]; then
    LIBUI="$(cd "$JETILANG/../libui-ng" && pwd)"
else
    echo "Error: libui-ng not found. Set LIBUI_DIR to your libui-ng checkout." >&2
    exit 1
fi

BUILD=/tmp/libui_build
rm -rf "$BUILD"
mkdir -p "$BUILD"

# Precompiled Foundation mode: the demo imports Foundation.hje (declarations
# only) and links the archive — the recommended multi-TU shape. Build it once
# if it is not there yet.
FOUNDATION_A="$JETILANG/target/foundation/libjetifoundation.a"
if [ ! -f "$FOUNDATION_A" ]; then
    echo "==> Building libjetifoundation.a (first run)..."
    "$JETILANG/tools/build-foundation-lib.sh" >&2
fi

echo "==> Transpile (jetic)..."
"$JETIC" -rewrite-jeti "$SCRIPT_DIR/libui_demo.jeti" -o "$BUILD/libui_demo.c" \
    -I "$SCRIPT_DIR/include" -I "$LIBUI"

echo "==> Compile + link (clang)..."
FLAGS="-std=c99 -fblocks -w"
INCLUDES=(-I "$JETILANG/include" -I "$JETILANG/include/Foundation" -I "$LIBUI")
if [ "$(uname)" = "Darwin" ]; then
    FRAMEWORKS=(-framework Cocoa)
else
    FRAMEWORKS=()
fi

clang $FLAGS "${INCLUDES[@]}" \
    -x c "$BUILD/libui_demo.c" \
    "$JETILANG/include/jeti/runtime.c" \
    -x none "$FOUNDATION_A" \
    -L "$LIBUI/build/meson-out" -lui \
    -Wl,-rpath,"$LIBUI/build/meson-out" \
    "${FRAMEWORKS[@]}" \
    -o "$BUILD/libui_demo"

echo "==> Run..."
"$BUILD/libui_demo"
