# jeti test_all.sh — delegate everything to parallel Python runner
# Usage: ./test_all.sh [-jN]

JOBS=1
ARGS=""
for arg in "$@"; do
    if [[ "$arg" =~ ^-j([0-9]+)$ ]]; then JOBS="${BASH_REMATCH[1]}"; fi
    ARGS="$ARGS $arg"
done

cd "$(dirname "$0")"

# Python env: test_all.py needs `rich` (system Pythons are PEP 668-locked on
# macOS). Use the project .venv, building it on first use; fall back to the
# system python3 when venv creation is impossible — the runner then fails on
# the import with its own clear error instead of one from here.
PY=.venv/bin/python
if [ ! -x "$PY" ]; then
    if python3 -m venv .venv 2>/dev/null && .venv/bin/pip -q install rich 2>/dev/null; then
        :
    else
        rm -rf .venv
        PY=python3
    fi
fi
"$PY" test_all.py $ARGS
# Kill any leftover jetic processes (orphaned if Python was killed by timeout)
pkill -f "target/debug/jetic" 2>/dev/null || true
pkill -f "target/release/jetic" 2>/dev/null || true
# Kill orphaned test binaries (compiled .jeti executables left in /tmp/)
for f in tests/*.jeti; do
    stem=$(basename "$f" .jeti)
    pkill -f "^/tmp/$stem($| )" 2>/dev/null || true
done
