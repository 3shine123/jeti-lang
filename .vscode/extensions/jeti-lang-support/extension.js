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
// The semantic-tokens change event the provider registration consumes; built
// in activate() where `vscode` is live.
let semanticTokensChanged = null;
/** @type {vscode.StatusBarItem | null} */
let status = null;

function log(message) {
  output?.appendLine(message);
}

/**
 * The status bar item clangd's extension shows ("clangd: idle") — for jeti:
 * which server state the editor is in and which file the analysis belongs to.
 * Never an error indicator: diagnostics live in the Problems panel; this only
 * answers "is the language server alive and watching this file".
 */
function ensureStatus() {
  if (status) return status;
  status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 10);
  status.name = 'jeti-lsp';
  status.tooltip = 'jeti-lsp language server';
  status.command = 'jeti.showServerLog';
  status.hide();
  return status;
}

function setServerStatus(phase, detail) {
  const bar = ensureStatus();
  bar.text = detail
    ? `$(sync~spinner) jeti-lsp: ${phase} · ${detail}`
    : `$(sync~spinner) jeti-lsp: ${phase}`;
  bar.show();
}

function hideStatusIfNoJetiTab() {
  const active = vscode.window.activeTextEditor;
  if (active && isJetiDocument(active.document)) return;
  status?.hide();
}

function refreshStatusForActiveTab() {
  const active = vscode.window.activeTextEditor;
  if (!active || !isJetiDocument(active.document)) {
    // Not a jeti/hje tab: the item has nothing to say about the file under
    // the cursor — hide it instead of showing the previous file's state.
    hideStatusIfNoJetiTab();
    return;
  }
  if (!client) {
    setServerStatus('stopped');
    return;
  }
  // The name is the baseneame of the file under the cursor, mirroring
  // clangd's per-file phase text.
  setServerStatus('idle', baseName(active.document.uri.fsPath));
}

function baseName(p) {
  return p.split('/').pop().split('\\').pop();
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
    // The build switches that change diagnostics. The editor has to agree with
    // the build, or it reports errors for code the compiler accepts (a legacy
    // MRC file's manual `release` is an ARC error under the compiler's
    // defaults, and none at all with `-fno-jeti-arc`).
    arc: config.get('arc') !== false,
    flags: config.get('arguments') || [],
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

function toRange(range) {
  const start = range?.start || { line: 0, character: 0 };
  const end = range?.end || start;
  // LSP positions are already 0-based line + UTF-16 character, which is exactly
  // what vscode.Position takes — no unit conversion belongs here.
  return new vscode.Range(start.line, start.character, end.line, end.character);
}

function toDiagnostic(diagnostic) {
  const converted = new vscode.Diagnostic(
    toRange(diagnostic.range),
    diagnostic.message || '',
    SEVERITY.get(diagnostic.severity) ?? vscode.DiagnosticSeverity.Error,
  );
  converted.source = diagnostic.source || 'jeti';
  return converted;
}

function toDocumentSymbol(symbol) {
  // Jeti's AST carries one extent per declaration, so the "whole" and the
  // "selected" range are the same thing.
  const range = toRange(symbol.range);
  const converted = new vscode.DocumentSymbol(
    symbol.name,
    symbol.detail || '',
    symbol.kind ?? vscode.SymbolKind.Variable,
    range,
    range,
  );
  converted.children = (symbol.children || []).map(toDocumentSymbol);
  return converted;
}

/** LSP `Location | Location[] | null` → VS Code's `Location[] | undefined`. */
function toLocations(locations) {
  if (!locations) return undefined;
  const list = Array.isArray(locations) ? locations : [locations];
  return list.map((location) => new vscode.Location(vscode.Uri.parse(location.uri), toRange(location.range)));
}

/** LSP `Hover | null` → VS Code's `Hover | undefined`. */
function toHover(hover) {
  if (!hover || !hover.contents) return undefined;
  const contents = hover.contents;
  const markdown = typeof contents === 'string' ? contents : contents.value || '';
  if (!markdown) return undefined;
  const range = hover.range ? toRange(hover.range) : undefined;
  return new vscode.Hover(new vscode.MarkdownString(markdown), range);
}

/** LSP `InlayHint[]` → VS Code's `InlayHint[]` (gray, hover-less). */
function toInlayHints(hints) {
  return (hints || []).map((hint) => {
    const converted = new vscode.InlayHint(
      new vscode.Position(hint.position.line, hint.position.character),
      hint.label,
    );
    // LSP InlayHintKind: 1 = Parameter, 2 = Type.
    if (hint.kind === 2) converted.kind = vscode.InlayHintKind.Type;
    else if (hint.kind === 1) converted.kind = vscode.InlayHintKind.Parameter;
    return converted;
  });
}

/** LSP `CompletionItem[]` → VS Code's `CompletionItem[]`. */
function toCompletionItems(items) {
  return (items || []).map((item) => {
    const converted = new vscode.CompletionItem(item.label, item.kind ?? vscode.CompletionItemKind.Text);
    if (item.detail) converted.detail = item.detail;
    return converted;
  });
}

/** LSP `SignatureHelp | null` → VS Code's `SignatureHelp | undefined`. */
function toSignatureHelp(help) {
  if (!help || !Array.isArray(help.signatures) || help.signatures.length === 0) return undefined;
  const converted = new vscode.SignatureHelp();
  converted.signatures = help.signatures.map((signature) => {
    const parameters = (signature.parameters || []).map(
      (parameter) => new vscode.ParameterInformation(parameter.label),
    );
    return new vscode.SignatureInformation(signature.label, undefined, parameters);
  });
  converted.activeSignature = help.activeSignature ?? 0;
  converted.activeParameter = help.activeParameter ?? 0;
  return converted;
}

function publish(uriString, diagnostics) {
  const uri = vscode.Uri.parse(uriString);
  collection?.set(uri, (diagnostics || []).map(toDiagnostic));
}

async function start() {
  const { serverPath, searchPaths, arc, flags } = settings();
  const root = workspaceRoot();
  const command = resolveServerPath(serverPath, root);
  const args = [];
  for (const dir of searchPaths) args.push('--include', resolveSearchPath(dir, root));
  // A project's own flags, in jetic's spelling (`-eh legacy`, `-fno-checker`, …).
  args.push(...flags);
  if (!arc) args.push('-fno-jeti-arc');
  const rootUri = root ? vscode.Uri.file(root).toString() : null;

  setServerStatus('starting');
  client = new JetiLanguageClient({
    command, args, cwd: root, onDiagnostics: publish, onLog: log,
    // The server asks for a re-pull after every analysis flush: a fetch that
    // raced the first analysis cached empty tokens, and only this makes the
    // editor fetch again.
    onSemanticTokensRefresh: () => semanticTokensChanged?.fire(),
  });
  try {
    const result = await client.start({ rootUri });
    const info = result?.serverInfo || {};
    log(`jeti-lsp started: ${command} ${args.join(' ')} (${info.name || 'jeti-lsp'} ${info.version || ''})`);
    refreshStatusForActiveTab();
    return true;
  } catch (error) {
    client.kill();
    client = null;
    setServerStatus('stopped');
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
  if (status && vscode.window.activeTextEditor && isJetiDocument(vscode.window.activeTextEditor.document)) {
    setServerStatus('stopped');
  }
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
      refreshStatusForActiveTab();
    })
    .catch((error) => log(`jeti restart failed: ${error.message}`));
  return pendingRestart;
}async function activate(context) {
  output = vscode.window.createOutputChannel('Jeti');
  collection = vscode.languages.createDiagnosticCollection('jeti');
  // Fired when the server asks for a semantic-tokens re-pull; handed to the
  // provider registration below as its change event.
  semanticTokensChanged = new vscode.EventEmitter();
  context.subscriptions.push(output, collection, semanticTokensChanged, { dispose: () => client?.kill() });

  // Registered before the first start so the handlers exist no matter how the
  // start goes; every one of them is a no-op while `client` is null.
  context.subscriptions.push(
    vscode.languages.registerDocumentSymbolProvider(LANGUAGE_ID, {
      provideDocumentSymbols: async (document) => {
        if (!client) return [];
        return (await client.documentSymbols(document.uri.toString())).map(toDocumentSymbol);
      },
    }),
    vscode.languages.registerDefinitionProvider(LANGUAGE_ID, {
      provideDefinition: async (document, position) => {
        if (!client) return undefined;
        const locations = await client.definition(document.uri.toString(), {
          line: position.line,
          character: position.character,
        });
        return toLocations(locations);
      },
    }),
    vscode.languages.registerHoverProvider(LANGUAGE_ID, {
      provideHover: async (document, position) => {
        if (!client) return undefined;
        return toHover(await client.hover(document.uri.toString(), {
          line: position.line,
          character: position.character,
        }));
      },
    }),
    vscode.languages.registerCompletionItemProvider(LANGUAGE_ID, {
      provideCompletionItems: async (document, position) => {
        if (!client) return undefined;
        const items = await client.completion(document.uri.toString(), {
          line: position.line,
          character: position.character,
        });
        return toCompletionItems(items);
      },
    // Trigger characters: without them VS Code only fires the provider on
    // word characters — a `-` (method prefix) or `:` (selector part) pops
    // the editor's built-in file/word suggestions instead of the server's
    // list. `(` and `,` keep signature help flowing through an argument
    // list (clangd registers the same pair).
    }, ':', '(', ',', '-'),
    vscode.languages.registerSignatureHelpProvider(
      LANGUAGE_ID,
      {
        provideSignatureHelp: async (document, position) => {
          if (!client) return undefined;
          return toSignatureHelp(await client.signatureHelp(document.uri.toString(), {
            line: position.line,
            character: position.character,
          }));
        },
      },
      ':', '(', ',', '-',
    ),
    // Include paths as links: underline, whole-path hover highlight, cmd+click
    // to the resolved file (clangd parity). Links without a target (nothing
    // resolved) render as plain text.
    vscode.languages.registerDocumentLinkProvider(LANGUAGE_ID, {
      provideDocumentLinks: async (document) => {
        if (!client) return [];
        return (await client.documentLinks(document.uri.toString()))
          .filter((link) => link?.target)
          .map((link) => new vscode.DocumentLink(toRange(link.range), vscode.Uri.parse(link.target)));
      },
    }),
    // Gray, hover-less hints over the source (clangd's parameter/type inlays).
    // The server advertises `inlayHintProvider`, but a custom client must bridge
    // the request itself — without this registration VS Code never asks and the
    // editor shows nothing, however many hints the server would return.
    vscode.languages.registerInlayHintsProvider(LANGUAGE_ID, {
      provideInlayHints: async (document, range) => {
        if (!client) return [];
        const hints = await client.inlayHints(document.uri.toString(), {
          start: { line: range.start.line, character: range.start.character },
          end: { line: range.end.line, character: range.end.character },
        });
        return toInlayHints(hints);
      },
    }),
    vscode.workspace.onDidOpenTextDocument((document) => {
      if (client && isJetiDocument(document)) {
        client.openDocument(document.uri.toString(), document.getText());
        refreshStatusForActiveTab();
      }
    }),
    // Full sync (the server advertises `textDocumentSync: 1`): always send the
    // whole buffer. The server re-analyzes from scratch on each flush, so
    // incremental ranges would buy nothing — and a range applied to a
    // desynchronized server-side copy is dropped, freezing the diagnostics on
    // stale content. A full replacement can never desync.
    vscode.workspace.onDidChangeTextDocument((event) => {
      if (!client || !isJetiDocument(event.document)) return;
      client.changeDocument(event.document.uri.toString(), [
        { text: event.document.getText() },
      ]);
    }),
    vscode.workspace.onDidCloseTextDocument((document) => {
      if (client && isJetiDocument(document)) {
        client.closeDocument(document.uri.toString());
      }
      collection?.delete(document.uri);
    }),
  );

  // The compiler's own coloring, on top of the grammar: the server knows
  // types, classes, fields, enum members and macros; the grammar keeps
  // keywords and punctuation. Legend order must match the server's. Kept out
  // of the registration block above on purpose — if this API call ever fails,
  // it must not take the document-event handlers (the LSP's lifeline) down
  // with it; coloring degrades to the grammar instead.
  // `registerDocumentSemanticTokensProvider` is the real API name (and
  // `provideDocumentSemanticTokens` the real provider method) — a misspelled
  // name throws here and the catch would silently leave the editor grammar-
  // only, which is exactly the "everything is white" failure mode.
  try {
    context.subscriptions.push(
      vscode.languages.registerDocumentSemanticTokensProvider(
        LANGUAGE_ID,
        {
          provideDocumentSemanticTokens: async (document) => {
            if (!client) return new vscode.SemanticTokens(new Uint32Array());
            const result = await client.semanticTokensFull(document.uri.toString());
            return new vscode.SemanticTokens(new Uint32Array(result?.data ?? []));
          },
        },
        new vscode.SemanticTokensLegend(
          // Must match the server legend exactly (lib.rs capabilities) — a
          // type index past this array makes VS Code drop the token.
          ['namespace', 'class', 'type', 'enum', 'function', 'variable', 'property', 'macro', 'enumMember', 'keyword'],
          ['static', 'defaultLibrary'],
        ),
        // The server-pushed refresh request fires this, making VS Code
        // re-invoke provideDocumentSemanticTokens for every visible editor.
        semanticTokensChanged.event,
      ),
    );
  } catch (error) {
    output?.appendLine(`semantic tokens unavailable: ${error?.message ?? error}`);
  }

  // Status bar + tab tracking. The active-editor hook fires on tab switch,
  // so switching to a .jeti/.hje tab shows the file the analysis belongs to
  // and switching away hides the item — the same behavior clangd's
  // "clangd: idle" item has for C files.
  const bar = ensureStatus();
  context.subscriptions.push(bar);
  context.subscriptions.push(
    vscode.window.onDidChangeActiveTextEditor(() => {
      // Switching away from a jeti/hje tab hides the item — the same behavior
      // clangd's "clangd: idle" item has for non-C files.
      hideStatusIfNoJetiTab();
      if (!client) return;
      refreshStatusForActiveTab();
    }),
    vscode.commands.registerCommand('jeti.showServerLog', () => output?.show()),
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

  if (started) refreshStatusForActiveTab();
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
