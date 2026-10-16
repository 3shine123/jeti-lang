#!/usr/bin/env bash
# Open VS Code with the Jeti extension + language server loaded, the way you
# would while working on the extension itself.
#
#   ./tools/run-vscode-extension.sh                 # this repository
#   ./tools/run-vscode-extension.sh /path/to/proj   # another Jeti project
#
# This launches an Extension Development Host: a second VS Code window with the
# extension from this checkout and no install into your own editor setup. Close
# that window when you are done. For a persistent install, package and install
# instead:
#
#   cd .vscode/extensions/jeti-lang-support && npx @vscode/vsce package
#   code --install-extension jeti-lang-<version>.vsix
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
extension_dir="$repo_root/.vscode/extensions/jeti-lang-support"
workspace="${1:-$repo_root}"

if ! command -v code >/dev/null 2>&1; then
    echo "run-vscode-extension: the 'code' command is not on PATH." >&2
    echo "  In VS Code: Command Palette -> 'Shell Command: Install code command in PATH'." >&2
    exit 1
fi

if [[ ! -x "$repo_root/target/debug/jeti-lsp" && ! -x "$repo_root/target/release/jeti-lsp" ]]; then
    echo "run-vscode-extension: building jeti-lsp first..."
    (cd "$repo_root" && cargo build -p jeti-lsp)
fi

echo "extension : $extension_dir"
echo "workspace : $workspace"
echo "server    : $repo_root/target/debug/jeti-lsp"
exec code --extensionDevelopmentPath="$extension_dir" "$workspace"
