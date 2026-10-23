#!/bin/bash
# tests/eh_diff/run_eh_diff.sh — jeti vs 真 ObjC 异常语义差分测试
#
# 每个用例是一对 X.jeti(jeti)+ X.m(等价 ObjC)。ObjC 侧用系统 clang 编译运行,
# 作为**语义基准**;jeti 侧用 jetic -eh checked 编译运行;两侧 stderr 逐行 diff,
# 不一致即 FAIL。异常语义的 ObjC 兼容性由此套件守护,而非口头保证。
#
# 用法: ./run_eh_diff.sh            # 跑全部(含 KNOWN-FAIL,单列统计)
#       JETIC=/path/to/jetic ./run_eh_diff.sh
#
# KNOWN-FAIL 标记(写在 .jeti 头部注释)的用例:当前预期 diff,单独计数 KFAIL,
# 对应功能落地后应删除标记使其转为真用例。
set -u
cd "$(dirname "$0")"
ROOT="$(cd ../.. && pwd)"                      # 仓库根
JETIC="${JETIC:-$ROOT/target/debug/jetic}"
EH_FLAG="${EH_FLAG:--eh checked}"
OBJCC="${OBJCC:-clang}"
WORK="${TMPDIR:-/tmp}/jeti_eh_diff"
mkdir -p "$WORK"

PASS=0; FAIL=0; KFAIL=0; SKIP=0
FAILED_CASES=""

# ObjC 二进制按内容缓存:改 .m 才重编
objc_bin() {
    local stem="$1" bin="$WORK/$stem.objc"
    if [ ! -x "$bin" ] || [ "$m" -nt "$bin" ]; then
        # -fobjc-arc-exceptions = 异常安全 ARC: unwind 经过的每一帧照常结算其
        # ARC owned 局部(jeti 的 ARC 天生如此)。默认的 -fobjc-arc 在 unwind 时
        # 会漏掉中间帧的 release —— 那是缺陷参照,不是语义基准。
        $OBJCC -fobjc-arc -fobjc-arc-exceptions -framework Foundation -o "$bin" "$m" 2>"$WORK/$stem.objc.cc" || {
            echo "OBJC-CC-FAIL $stem"; cat "$WORK/$stem.objc.cc"; return 1; }
    fi
    echo "$bin"
}

for np in *.jeti; do
    stem="${np%.jeti}"
    m="$stem.m"
    if [ ! -f "$m" ]; then
        echo "SKIP   $stem (no .m pair)"; SKIP=$((SKIP+1)); continue
    fi

    # ObjC 基准:编译 + 运行(stderr 与退出码)
    bin="$(objc_bin "$stem")" || { FAIL=$((FAIL+1)); FAILED_CASES="$FAILED_CASES $stem"; continue; }
    "$bin" 2>"$WORK/$stem.objc.out"; objc_rc=$?

    # jeti 侧:run 模式(与用户路径一致),捕获合并输出
    "$JETIC" run "$np" $EH_FLAG >"$WORK/$stem.jeti.all" 2>&1; jeti_rc=$?
    # 只取程序 stderr(NPLog 走 stderr;jetic 自身的 stdout/警告已在 all 里,按需剔除)
    # jetic 自身的诊断一律带 ANSI 前缀(`\x1b[1;31merror:` / `\x1b[1;33mwarning:`,见各
    # crate 的 eprintln 配色),且 jetic 不做 isatty/NO_COLOR 判断,重定向下照发;而程序
    # 输出经 NPLog 是裸写 stderr、不带转义。所以按 ESC 前缀剔除就能精确去掉 jetic 的行
    # —— 包括 `run` 遇到子进程被信号杀死时新报的 "terminated by signal N" —— 又不会
    # 误伤程序自己打印的 "error: ..." 文本。
    grep -v $'^\033' "$WORK/$stem.jeti.all" > "$WORK/$stem.jeti.out" || true

    # 07_uncaught:全栈回溯与 Foundation 内部类名(如 '__NSCFConstantString' vs
    # 'NPString')不可复现,只比措辞前缀 "*** Terminating app due to uncaught exception of class '"
    cmp_from="$WORK/$stem.jeti.out"; cmp_to="$WORK/$stem.objc.out"
    if [ "$stem" = "07_uncaught" ]; then
        grep '^\*\*\* Terminating' "$WORK/$stem.jeti.out" | sed "s/class '[^']*'/class 'X'/" > "$WORK/$stem.jeti.f" 2>/dev/null
        grep '^\*\*\* Terminating' "$WORK/$stem.objc.out" | sed "s/class '[^']*'/class 'X'/" > "$WORK/$stem.objc.f" 2>/dev/null
        cmp_from="$WORK/$stem.jeti.f"; cmp_to="$WORK/$stem.objc.f"
    fi

    known_fail=0
    grep -q "KNOWN-FAIL" "$np" && known_fail=1

    if diff -u "$cmp_to" "$cmp_from" >"$WORK/$stem.diff" 2>&1; then
        echo "PASS   $stem"
        PASS=$((PASS+1))
    else
        if [ "$known_fail" = 1 ]; then
            echo "KFAIL  $stem (known-fail: diff below)"
            sed 's/^/         /' "$WORK/$stem.diff" | head -12
            KFAIL=$((KFAIL+1))
        else
            echo "FAIL   $stem (jeti_rc=$jeti_rc objc_rc=$objc_rc; diff below)"
            sed 's/^/         /' "$WORK/$stem.diff" | head -12
            FAIL=$((FAIL+1)); FAILED_CASES="$FAILED_CASES $stem"
        fi
    fi
done

echo "----------------------------------------"
echo "pass=$PASS fail=$FAIL kfail=$KFAIL skip=$SKIP"
[ -n "$FAILED_CASES" ] && echo "failed:$FAILED_CASES"
[ "$FAIL" -eq 0 ]
