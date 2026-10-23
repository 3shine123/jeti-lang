# 25_process_info — NPProcessInfo 补齐

P1-4：补齐 ObjC `NSProcessInfo` 的三块 —— `environment`（完整环境变量表）、
`globallyUniqueString`、`operatingSystemVersion` /
`isOperatingSystemAtLeastVersion:` / `operatingSystemVersionString`。

## 判据为什么是「结构性质」而不是具体值

OS 版本值随机器变，唯一串每次调用都变（唯一性就是它的契约）。把它们打进
`.out` 会让期望输出不可复现。所以本用例断言的是**结构性质**：

- 形状是否合规（`globallyUniqueString` 的 UUID/pid/序号三段）；
- 两次调用是否不同（唯一性）；
- 大小关系是否正确（`isOperatingSystemAtLeastVersion:`）；
- 前缀是否存在（`operatingSystemVersionString` 的 `Version `）。

具体值的对齐由 clang ObjC 基准探针完成（见下），不是靠硬编码。

## 语义来源：clang ObjC 基准

`NSProcessInfo` 实测形状：

| 项 | ObjC 实测 |
| --- | --- |
| `globallyUniqueString` | `238B27A6-7753-42C2-84AE-6365F5EB756A-86625-00000124B5DE71C3`（`<UUID 大写>-<pid>-<16 位十六进制>`） |
| `operatingSystemVersion` | `27.2.0`（字段 `majorVersion`/`minorVersion`/`patchVersion`） |
| `operatingSystemVersionString` | `Version 27.2 (Build 26B5101f)`（patch 为 0 时不写出） |
| `environment` | 含 `PATH`，条数 > 0 |

落地对齐：`NPROperatingSystemVersion` 字段名与 ObjC 逐一对齐；
`globallyUniqueString` 复用 `NPUUID` 的 v4 生成并转大写，尾部是
`<pid>-<16 位十六进制序号>`（进程内单调递增 + 首次调用的微秒时间戳）。

## 覆盖

| 组 | 覆盖 |
| --- | --- |
| A1 | `environment` 是字典、含 `PATH`、条数 > 0 |
| A2 | `globallyUniqueString` 形状合规 |
| A3 | 两次调用不同（唯一性） |
| A4 | `operatingSystemVersion` 主版本 > 0（确实从 OS 取到） |
| A5 | `isOperatingSystemAtLeastVersion:`：`0.0.0` 为真、`99.0.0` 为假 |
| A6 | `operatingSystemVersionString` 以 `Version ` 开头 |
| A7 | `arguments`：非空、`[0]` 是程序路径、元素都是 `NPString`、条数与 `jeti_argc()` 一致 |

## `arguments` 怎么拿到的（运行期钩子）

`NSProcessInfo.arguments` 在 ObjC 里直接读进程的 argv。Jeti 这边 **jetic 不合成
`main`**（由用户书写 `int main(int argc, char **argv)`），所以库内部无从拿到
`main` 的参数，只能由**运行期**从平台侧捕获 —— 为此在 `jeti/runtime.h` 加了两个
钩子：

| 平台 | 捕获路径 |
| --- | --- |
| Apple | `_NSGetArgc` / `_NSGetArgv` |
| Linux | `/proc/self/cmdline`（NUL 分隔，读一次后缓存切分） |
| 其他 | 空（argc 0）—— **文档化的平台差异，不是静默猜值** |

`freestanding` 版返回空（没有进程也就没有参数向量）。下标 0 是程序路径，与 ObjC
一致。`A7` 的 `matchesRuntime` 判据把 Foundation 层与运行期钩子绑在一起：两边
条数必须相等，任何一侧漏算都会现形。

多参数情形已人工验证（`jetic run prog.jeti marker1 marker2` → 三个参数如实入列）；
自动化用例不带额外参数运行，所以只断言与参数个数无关的结构性质。

## 明确边界

`-arguments` 现已提供（见上）。除此之外本类仍是 ObjC `NSProcessInfo` 的可用子集：
`userName`/`hostName`/`operatingSystem`/`machine`/环境变量读写齐备；进程名设置
（`-setProcessName:`）、`NSProcessInfo` 的电源/活动相关 API 不在范围内。

## 连带修复的编译器 Bug：文件级 `extern` 变量丢失存储类

本用例是库里**第一个**文件级 `extern` **变量**（此前只有 `extern` 函数，走
`ExternFunc` 路径）。首版 `extern char **environ;` 被发射成：

```c
char * * environ;
```

即丢了 `extern`，变成零初始化的 **tentative definition**，把 libc 的 `environ`
遮蔽成 NULL —— 环境表读出来是空的。`AstDeclData::Variable` 与 elaborator 都保留
了 `is_extern`，丢在 codegen：`CgDeclData::Variable` 压根没有该字段。已补上字段
并在两处发射点输出 `extern `。
