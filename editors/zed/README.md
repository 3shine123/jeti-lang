# Jeti for Zed

Jeti 语言支持：tree-sitter 语法高亮、outline、括号匹配，以及 `jeti-lsp` 语言
服务器（诊断、inlay hints、go-to-definition、signature help）。

## 目录结构

```
editors/zed/
  extension.toml            # 清单：grammar + language server 注册
  Cargo.toml + src/lib.rs   # wasm 扩展：解析并启动 jeti-lsp
  languages/jeti/
    config.toml             # 文件类型、注释、括号
    highlights.scm          # 高亮查询（grammar/queries 的副本）
  grammar/                  # tree-sitter-jeti（独立 git 仓库，Zed 会 clone 它）
    grammar.js
    queries/highlights.scm
    src/                    # tree-sitter generate 的产物（parser.c 等）
```

## 安装（开发版）

1. 构建 LSP（扩展按「workspace target/ → $PATH」顺序查找）：

   ```sh
   cargo build -p jeti-lsp
   ```

2. Zed 里运行 `zed: install dev extension`，选择本目录（`editors/zed`）。

3. 打开任意 `.jeti` 文件。若高亮缺失或 LSP 未启动，从命令行跑
   `zed --foreground` 看日志。

## grammar 的 `file://` 指向

`extension.toml` 里 `[grammars.jeti]` 目前指向本机绝对路径的 `file://` URL
（Zed 要求 grammar 来自一个 git 仓库并按 commit 取用）。发布前：

1. 把 `grammar/`（它自身是个 git 仓库）推到 GitHub；
2. 把 `repository` 改成仓库 URL，`commit` 改成对应 commit SHA；
3. 同步更新 `languages/jeti/highlights.scm`（`cp grammar/queries/highlights.scm languages/jeti/`）。

## 修改语法后

```sh
cd grammar
tree-sitter generate          # 重新生成 parser
tree-sitter parse ../../tests/demo.jeti   # 冒烟
git add -A && git commit -m "..."         # Zed 按 commit SHA 取用
# 把新 SHA 填进 extension.toml，再在 Zed 里 rebuild dev extension
```

## LSP 参数

扩展按 VS Code 客户端的默认习惯传 `--include <root>/include --include <root>`；
`compile_flags.txt` 由 `jeti-lsp` 自行读取（`-fno-jeti-arc`、`-fno-inlay-hints`
等旗标三个入口通用：compile_flags.txt / `jeti.arguments` 等价的命令行 / CLI）。
