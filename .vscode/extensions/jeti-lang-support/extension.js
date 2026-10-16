'use strict';

/**
 * VS Code glue for `jeti-lsp`.
 *
 * Everything protocol-shaped lives in `lsp-client.js` (no `vscode` import, so
 * `test/client.test.js` can drive it against the real server). This file only:
 *
 *   - decides which executable and `--include` dirs to use,
 *   - forwards this editor's document events (full-document sync),
 *   - renders `publishDiagnostics` into a DiagnosticCollection.
 *
 * Diagnostics are the compiler's own (`Pipeline::analyze`), so what shows up
 * here is what `jetic` reports on the command line.
 */

const fs = require('fs');
const path = require('path');
const vscode = require('vscode');
const { JetiLanguageClient } = require('./lsp-client');

const LANGUAGE_ID = 'jeti';
const JETI_EXTENSIONS = ['.jeti', '.hje'];

/** LSP severity numbers → VS Code's enum. */
const SEVERITY = new Map([
  [1, vscode.DiagnosticSeverity.Error],
  [2, vscode.DiagnosticSeverity.Warning],
  [3, vscode.DiagnosticSeverity.Information],
  [4, vscode.DiagnosticSeverity.Hint],
]);

/** @type {JetiLanguageClient | null} */
let client = null;
/** @type {vscode.DiagnosticCollection | null} */
let collection = null;
/** @type {vscode.OutputChannel | null} */
let output = null;

function log(message) {
  output?.appendLine(message);
}

function isJetiDocument(document) {
  if (document.languageId === LANGUAGE_ID) return true;
  const path_ = document.uri.fsPath;
  return JETI_EXTENSIONS.some((ext) => path_.endsWith(ext));
}

function settings() {
  const config = vscode.workspace.getConfiguration('jeti');
  return {
    serverPath: config.get('serverPath') || 'jeti-lsp',
    searchPaths: config.get('searchPaths') || ['include', '.'],
  };
}

function workspaceRoot() {
  const folders = vscode.workspace.workspaceFolders;
  return folders && folders.length ? folders[0].uri.fsPath : undefined;
}

/**
 * The executable to spawn.
 *
 * An explicit path setting (absolute, or relative to the workspace folder) is
 * used as given. A bare name means "the server": this repository's own build
 * output wins so `F5` in the Jeti repo just works, otherwise the name is left
 * for the OS to resolve on `PATH`.
 */
function resolveServerPath(serverPath, root) {
  const pathLike = path.isAbsolute(serverPath) || serverPath.includes(path.sep) || serverPath.startsWith('.');
  if (pathLike) {
    return path.isAbsolute(serverPath) ? serverPath : path.join(root || '.', serverPath);
  }
  if (root) {
    for (const candidate of ['target/debug/jeti-lsp', 'target/release/jeti-lsp']) {
      const full = path.join(root, candidate);
      if (fs.existsSync(full)) return full;
    }
  }
  return serverPath;
}

function resolveSearchPath(dir, root) {
  if (path.isAbsolute(dir)) return dir;
  return root ? path.join(root, dir) : dir;
}

function toDiagnostic(diagnostic) {
  const range = diagnostic.range || {};
  const start = range.start || { line: 0, character: 0 };
  const end = range.end || start;
  // LSP positions are already 0-based line + UTF-16 character, which is exactly
  // what vscode.Position takes — no unit conversion belongs here.
  const converted = new vscode.Diagnostic(
    new vscode.Range(start.line, start.character, end.line, end.character),
    diagnostic.message || '',
    SEVERITY.get(diagnostic.severity) ?? vscode.DiagnosticSeverity.Error,
  );
  converted.source = diagnostic.source || 'jeti';
  return converted;
}

function publish(uriString, diagnostics) {
  const uri = vscode.Uri.parse(uriString);
  collection?.set(uri, (diagnostics || []).map(toDiagnostic));
}

async function start() {
  const { serverPath, searchPaths } = settings();
  const root = workspaceRoot();
  const command = resolveServerPath(serverPath, root);
  const args = [];
  for (const dir of searchPaths) args.push('--include', resolveSearchPath(dir, root));
  const rootUri = root ? vscode.Uri.file(root).toString() : null;

  client = new JetiLanguageClient({ command, args, cwd: root, onDiagnostics: publish, onLog: log });
  try {
    const result = await client.start({ rootUri });
    const info = result?.serverInfo || {};
    log(`jeti-lsp started: ${command} ${args.join(' ')} (${info.name || 'jeti-lsp'} ${info.version || ''})`);
    return true;
  } catch (error) {
    client.kill();
    client = null;
    log(`jeti-lsp failed to start: ${error.message}`);
    vscode.window.showWarningMessage(
      `Jeti: language server failed to start (${error.message}). Build it with "cargo build -p jeti-lsp" or set "jeti.serverPath".`,
    );
    return false;
  }
}

async function stop() {
  const running = client;
  client = null;
  if (!running) return;
  await running.stop().catch(() => running.kill());
}

/**
 * Serialized so a burst of settings changes cannot spawn two servers at once —
 * an interleaved pair would have the second `stop()` kill the first one's
 * replacement, leaking the other.
 */
let pendingRestart = Promise.resolve();

function restart() {
  pendingRestart = pendingRestart
    .then(async () => {
      await stop();
      collection?.clear();
      await start();
    })
    .catch((error) => log(`jeti restart failed: ${error.message}`));
  return pendingRestart;
}async function activate(context) {
  output = vscode.window.createOutputChannel('Jeti');
  collection = vscode.languages.createDiagnosticCollection('jeti');
  context.subscriptions.push(output, collection, { dispose: () => client?.kill() });

  // Registered before the first start so the handlers exist no matter how the
  // start goes; every one of them is a no-op while `client` is null.
  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument((document) => {
      if (client && isJetiDocument(document)) {
        client.openDocument(document.uri.toString(), document.getText());
      }
    }),
    // Full-document sync (the server advertises `textDocumentSync: 1`), so the
    // whole buffer travels on every keystroke batch.
    vscode.workspace.onDidChangeTextDocument((event) => {
      if (client && isJetiDocument(event.document)) {
        client.changeDocument(event.document.uri.toString(), event.document.getText());
      }
    }),
    vscode.workspace.onDidCloseTextDocument((document) => {
      if (client && isJetiDocument(document)) {
        client.closeDocument(document.uri.toString());
      }
      collection?.delete(document.uri);
    }),
  );

  const started = await start();

  // Registered after the first start so it can never interleave with it — but
  // before bailing out, so a settings change can revive a failed server.
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration(async (event) => {
      if (event.affectsConfiguration('jeti')) {
        log('jeti settings changed — restarting the language server');
        await restart();
      }
    }),
  );

  if (!started) return;

  // Documents already open when the extension activated.
  for (const document of vscode.workspace.textDocuments) {
    if (client && isJetiDocument(document)) {
      client.openDocument(document.uri.toString(), document.getText());
    }
  }
}

async function deactivate() {
  await pendingRestart;
  await stop();
}

module.exports = { activate, deactivate };
