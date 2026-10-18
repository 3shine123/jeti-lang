'use strict';

/**
 * Drives `lsp-client.js` against the real `jeti-lsp` binary, the way the
 * extension drives it — no VS Code needed, because the transport layer has no
 * `vscode` import. This is the end-to-end check that the extension's half of
 * the wiring works: open a buffer, get diagnostics, edit it clean, close it.
 *
 *   cargo build -p jeti-lsp
 *   node .vscode/extensions/jeti-lang-support/test/client.test.js
 *
 * `JETI_LSP=/path/to/jeti-lsp` overrides the binary under test.
 */

const assert = require('assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { pathToFileURL } = require('url');

const { JetiLanguageClient } = require('../lsp-client');

const REPO_ROOT = path.resolve(__dirname, '..', '..', '..', '..');
const SERVER = process.env.JETI_LSP || path.join(REPO_ROOT, 'target', 'debug', 'jeti-lsp');

/** Latest diagnostics per uri plus waiters, so a publish that lands before the
 *  assertion still counts. */
function createFeed() {
  const latest = new Map();
  const waiters = [];
  return {
    onDiagnostics(uri, diagnostics) {
      latest.set(uri, diagnostics);
      for (const waiter of [...waiters]) {
        if (waiter.uri !== uri || !waiter.predicate(diagnostics)) continue;
        waiters.splice(waiters.indexOf(waiter), 1);
        waiter.settle(diagnostics);
      }
    },
    waitFor(uri, predicate, timeoutMs = 20000) {
      const current = latest.get(uri);
      if (current && predicate(current)) return Promise.resolve(current);
      return new Promise((resolve, reject) => {
        const waiter = { uri, predicate, settle: null };
        const timer = setTimeout(() => {
          waiters.splice(waiters.indexOf(waiter), 1);
          reject(new Error(`timed out waiting for diagnostics on ${uri}`));
        }, timeoutMs);
        timer.unref?.();
        waiter.settle = (diagnostics) => {
          clearTimeout(timer);
          resolve(diagnostics);
        };
        waiters.push(waiter);
      });
    },
  };
}

async function main() {
  if (!fs.existsSync(SERVER)) {
    console.error(`FAIL: no server at ${SERVER} — run "cargo build -p jeti-lsp" first (or set JETI_LSP)`);
    process.exitCode = 1;
    return;
  }

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'jeti-lsp-vscode-'));
  const file = path.join(dir, 'probe.jeti');
  const broken = 'int main(void) { return 0 }\n';
  fs.writeFileSync(file, broken);
  const uri = pathToFileURL(file).toString();

  const feed = createFeed();
  const logs = [];
  const client = new JetiLanguageClient({
    command: SERVER,
    args: ['--include', path.join(REPO_ROOT, 'include')],
    cwd: REPO_ROOT,
    onDiagnostics: feed.onDiagnostics,
    onLog: (text) => logs.push(text),
  });

  try {
    // The extension's start(): initialize handshake before any document event.
    const init = await client.start({ rootUri: pathToFileURL(REPO_ROOT).toString() });
    assert.strictEqual(init.capabilities.textDocumentSync, 1, 'expected full document sync');
    assert.strictEqual(init.capabilities.documentSymbolProvider, true, 'expected an outline provider');
    assert.strictEqual(init.capabilities.definitionProvider, true, 'expected a definition provider');
    assert.strictEqual(init.capabilities.hoverProvider, true, 'expected a hover provider');
    assert.strictEqual(init.capabilities.completionProvider.resolveProvider, false, 'expected inline completions');
    assert.deepStrictEqual(
      init.capabilities.signatureHelpProvider.triggerCharacters,
      [':'],
      'expected signature help on the keyword colon',
    );
    assert.strictEqual(init.serverInfo.name, 'jeti-lsp');
    console.log('ok  initialize handshake');

    // didOpen → publishDiagnostics with the syntax error at line 0, char 25:
    // clang's insertion-point caret sits where the `;` belongs, before the `}`.
    client.openDocument(uri, broken);
    const reported = await feed.waitFor(uri, (diagnostics) => diagnostics.length > 0);
    const first = reported[0];
    assert.strictEqual(first.severity, 1, `expected an error, got ${JSON.stringify(first)}`);
    assert.deepStrictEqual(first.range.start, { line: 0, character: 25 });
    assert.match(first.message, /expected ';'/);
    assert.strictEqual(first.source, 'jeti');
    console.log(`ok  didOpen → ${reported.length} diagnostic(s): ${first.message}`);

    // didChange with the whole buffer (full sync) clears them: the same line,
    // with the `;` before the `}`.
    client.changeDocument(uri, [{ text: 'int main(void) { return 0; }\n' }]);
    await feed.waitFor(uri, (diagnostics) => diagnostics.length === 0);
    console.log('ok  didChange (whole buffer) → diagnostics cleared');

    // semanticTokens → the compiler's coloring: `main` classifies as a
    // function (type 4 in the legend), keywords stay grammar-colored.
    const tokens = await client.semanticTokensFull(uri);
    assert.ok(Array.isArray(tokens?.data) && tokens.data.length >= 5, 'expected semantic tokens');
    assert.strictEqual(tokens.data[3], 4, `expected main as function, got ${JSON.stringify(tokens.data)}`);
    console.log(`ok  semanticTokens → ${tokens.data.length / 5} token(s), main = function`);

    // didClose clears the entry as well and must not resurrect it.
    client.closeDocument(uri);
    await feed.waitFor(uri, (diagnostics) => diagnostics.length === 0);
    console.log('ok  didClose → cleared');

    const code = await client.stop();
    assert.strictEqual(code, 0, 'clean shutdown should exit 0');
    console.log('ok  shutdown/exit → exit code 0');
  } catch (error) {
    process.exitCode = 1;
    console.error(`FAIL: ${error.message}`);
    if (logs.length) console.error(`server log:\n${logs.join('')}`);
    client.kill();
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

main().then(() => {
  if (process.exitCode !== 1) console.log('\nclient.test.js: all checks passed');
});
