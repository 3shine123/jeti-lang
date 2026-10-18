'use strict';

/**
 * Verifies the extension glue in `extension.js` — settings resolution, the
 * document-event wiring, and the LSP→VS Code diagnostic conversion — without
 * VS Code: `require('vscode')` is intercepted and answered with a small stub,
 * and the document events are fired by hand. The server underneath is the real
 * `jeti-lsp` binary, so this covers the whole path an editor takes.
 *
 *   cargo build -p jeti-lsp
 *   node .vscode/extensions/jeti-lang-support/test/extension.test.js
 *
 * `JETI_LSP=/path/to/jeti-lsp` overrides the binary (as `jeti.serverPath`).
 */

const assert = require('assert');
const fs = require('fs');
const Module = require('module');
const os = require('os');
const path = require('path');
const { pathToFileURL } = require('url');

const REPO_ROOT = path.resolve(__dirname, '..', '..', '..', '..');
const SERVER = process.env.JETI_LSP || path.join(REPO_ROOT, 'target', 'debug', 'jeti-lsp');

// ---------------------------------------------------------------------------
// The `vscode` stub. Only what extension.js touches.
// ---------------------------------------------------------------------------

const warnings = [];
const outputLines = [];
/** Status bar items the fake `vscode.window` handed out, for assertions. */
const statusItems = [];
const collections = new Map();
const listeners = new Map();
const providers = new Map();

const vscode = {
  DiagnosticSeverity: { Error: 0, Warning: 1, Information: 2, Hint: 3 },
  // The status bar API is top-level in VS Code (vscode.StatusBarAlignment),
  // which is how extension.js reads it.
  StatusBarAlignment: { Left: 1, Right: 2 },
  // LSP CompletionItemKind: Method 2, Function 3, Field 5, Variable 6, Class 7,
  // Interface 8, Module 9, Property 10, Enum 13, Struct 22, Text 1.
  CompletionItemKind: { Text: 1, Method: 2, Function: 3, Field: 5, Variable: 6, Class: 7, Interface: 8, Module: 9, Property: 10, Enum: 13, Struct: 22 },
  MarkdownString: class MarkdownString {
    constructor(value) {
      this.value = value;
    }
  },
  Hover: class Hover {
    constructor(contents, range) {
      this.contents = contents;
      this.range = range;
    }
  },
  CompletionItem: class CompletionItem {
    constructor(label, kind) {
      this.label = label;
      this.kind = kind;
      this.detail = undefined;
    }
  },
  ParameterInformation: class ParameterInformation {
    constructor(label) {
      this.label = label;
    }
  },
  SignatureInformation: class SignatureInformation {
    constructor(label, documentation, parameters) {
      this.label = label;
      this.documentation = documentation;
      this.parameters = parameters || [];
    }
  },
  SignatureHelp: class SignatureHelp {
    constructor() {
      this.signatures = [];
      this.activeSignature = 0;
      this.activeParameter = 0;
    }
  },
  SymbolKind: {
    Namespace: 3,
    Class: 5,
    Method: 6,
    Property: 7,
    Field: 8,
    Enum: 10,
    Interface: 11,
    Function: 12,
    Variable: 13,
    Struct: 23,
  },
  Position: class Position {
    constructor(line, character) {
      this.line = line;
      this.character = character;
    }
  },
  Range: class Range {
    constructor(startLine, startCharacter, endLine, endCharacter) {
      this.start = { line: startLine, character: startCharacter };
      this.end = { line: endLine, character: endCharacter };
    }
  },
  Diagnostic: class Diagnostic {
    constructor(range, message, severity) {
      this.range = range;
      this.message = message;
      this.severity = severity;
      this.source = undefined;
    }
  },
  DocumentSymbol: class DocumentSymbol {
    constructor(name, detail, kind, range, selectionRange) {
      this.name = name;
      this.detail = detail;
      this.kind = kind;
      this.range = range;
      this.selectionRange = selectionRange;
      this.children = [];
    }
  },
  Location: class Location {
    constructor(uri, range) {
      this.uri = uri;
      this.range = range;
    }
  },
  Uri: {
    parse: (text) => ({ text, toString: () => text }),
    file: (p) => ({ text: pathToFileURL(p).toString(), toString() { return this.text; }, fsPath: p }),
  },
  languages: {
    createDiagnosticCollection: (name) => {
      const store = new Map();
      collections.set(name, store);
      return {
        set: (uri, diagnostics) => store.set(uri.toString(), diagnostics),
        delete: (uri) => store.delete(uri.toString()),
        clear: () => store.clear(),
        dispose: () => store.clear(),
      };
    },
    registerDocumentSymbolProvider: (selector, provider) => registerProvider('documentSymbol', provider),
    registerDefinitionProvider: (selector, provider) => registerProvider('definition', provider),
    registerHoverProvider: (selector, provider) => registerProvider('hover', provider),
    registerCompletionItemProvider: (selector, provider) => registerProvider('completion', provider),
    registerSignatureHelpProvider: (selector, provider) => registerProvider('signatureHelp', provider),
  },
  commands: {
    registerCommand: (id, _handler) => ({ id, dispose: () => {} }),
  },
  window: {
    createOutputChannel: (name) => ({
      append: (text) => outputLines.push(text),
      appendLine: (line) => outputLines.push(line),
      dispose: () => {},
    }),
    showWarningMessage: (message) => warnings.push(message),
    // Status bar: tests read `statusItems` to assert the server phases.
    createStatusBarItem: (alignment, priority) => {
      const item = { text: '', name: '', tooltip: undefined, command: undefined, shown: false };
      statusItems.push(item);
      return {
        get text() { return item.text; },
        set text(v) { item.text = v; },
        get name() { return item.name; },
        set name(v) { item.name = v; },
        set tooltip(v) { item.tooltip = v; },
        get command() { return item.command; },
        set command(v) { item.command = v; },
        show: () => { item.shown = true; },
        hide: () => { item.shown = false; },
        dispose: () => {},
      };
    },
    activeTextEditor: null,
    onDidChangeActiveTextEditor: (listener) => subscribe('activeEditor', listener),
    registerCommand: (id, _handler) => ({ id, dispose: () => {} }),
  },
  workspace: {
    workspaceFolders: [{ uri: { fsPath: REPO_ROOT, toString: () => pathToFileURL(REPO_ROOT).toString() } }],
    textDocuments: [],
    // Defaults mirror package.json; `overrides` lets a test change a setting.
    getConfiguration: () => ({
      get: (key) => overrides[key] ?? defaults[key],
    }),
    onDidOpenTextDocument: (listener) => subscribe('open', listener),
    onDidChangeTextDocument: (listener) =>
      subscribe('change', (event) => {
        // VS Code updates the buffer before change listeners run; model that
        // so `getText()` inside a listener sees the new content.
        if (event.document && event.document.applyEdits) {
          event.document.applyEdits(event.contentChanges || []);
        }
        listener(event);
      }),
    onDidCloseTextDocument: (listener) => subscribe('close', listener),
    onDidChangeConfiguration: (listener) => subscribe('config', listener),
  },
};

const defaults = {
  serverPath: 'jeti-lsp',
  searchPaths: ['include', '.'],
  arc: true,
  arguments: [],
};
const overrides = {};

function subscribe(kind, listener) {
  if (!listeners.has(kind)) listeners.set(kind, []);
  listeners.get(kind).push(listener);
  return { dispose: () => {} };
}

function registerProvider(kind, provider) {
  providers.set(kind, provider);
  return { dispose: () => {} };
}

function fire(kind, event) {
  for (const listener of listeners.get(kind) || []) listener(event);
}

// Intercept `require('vscode')` before extension.js is loaded.
const originalLoad = Module._load;
Module._load = function (request, ...rest) {
  if (request === 'vscode') return vscode;
  return originalLoad.call(this, request, ...rest);
};

const extension = require('../extension.js');

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async function waitUntil(predicate, what, timeoutMs = 20000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = predicate();
    if (value) return value;
    if (Date.now() > deadline) {
      throw new Error(`timed out waiting for ${what}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}

function documentFor(file, text) {
  const state = { text };
  return {
    uri: { fsPath: file, toString: () => pathToFileURL(file).toString() },
    languageId: 'jeti',
    getText: () => state.text,
    // Apply the editor's content changes to the buffer (in reverse, so the
    // earlier positions stay valid), as VS Code does before notifying.
    applyEdits(changes) {
      const edits = changes
        .map((change) => ({
          start: change.range ? positionToIndex(state.text, change.range.start) : 0,
          end: change.range ? positionToIndex(state.text, change.range.end) : state.text.length,
          text: change.text,
        }))
        .sort((a, b) => b.start - a.start);
      for (const edit of edits) {
        state.text = state.text.slice(0, edit.start) + edit.text + state.text.slice(edit.end);
      }
    },
  };
}

/** (0-based line, UTF-16 character) → index into the string (UTF-16 units). */
function positionToIndex(text, position) {
  const lines = text.split('\n');
  if (position.line >= lines.length) return text.length;
  let index = 0;
  for (let i = 0; i < position.line; i += 1) index += lines[i].length + 1;
  return index + Math.min(position.character, lines[position.line].length);
}

/** The (0-based line, UTF-16 character) an editor reports for `needle`, shifted
 *  `extra` characters into it. */
function positionOf(text, needle, extra = 0) {
  const index = text.indexOf(needle);
  if (index < 0) throw new Error(`needle not found: ${needle}`);
  const target = index + extra;
  const line = text.slice(0, target).split('\n').length - 1;
  const lineStart = text.lastIndexOf('\n', target - 1) + 1;
  // JS strings are UTF-16, so their length *is* the LSP character count.
  return new vscode.Position(line, target - lineStart);
}

// ---------------------------------------------------------------------------
// The scenario
// ---------------------------------------------------------------------------

async function main() {
  if (!fs.existsSync(SERVER)) {
    console.error(`FAIL: no server at ${SERVER} — run "cargo build -p jeti-lsp" first (or set JETI_LSP)`);
    process.exitCode = 1;
    return;
  }
  if (process.env.JETI_LSP) overrides.serverPath = process.env.JETI_LSP;

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'jeti-lsp-ext-'));
  const file = path.join(dir, 'probe.jeti');
  const broken = 'int main(void) { return 0 }\n';
  const fixed = 'int main(void) { return 0; }\n';
  const uri = pathToFileURL(file).toString();
  fs.writeFileSync(file, broken);

  const context = { subscriptions: [] };
  try {
    await extension.activate(context);
    assert.deepStrictEqual(warnings, [], `unexpected warning: ${warnings.join('; ')}`);
    assert.ok(
      outputLines.some((line) => line.includes('jeti-lsp started')),
      `expected a start log line, got: ${outputLines.join(' | ')}`,
    );
    assert.ok(
      outputLines.some((line) => line.includes(path.join('target', 'debug', 'jeti-lsp'))),
      'a bare "jeti-lsp" setting should prefer the workspace build output',
    );
    console.log('ok  activate → server started from the workspace build output');

    const collection = () => collections.get('jeti');
    assert.ok(collection(), 'expected a "jeti" diagnostic collection');

    // didOpen → diagnostics rendered into the collection (LSP → VS Code).
    fire('open', documentFor(file, broken));
    const diagnostics = await waitUntil(() => {
      const entries = collection().get(uri);
      return entries && entries.length ? entries : null;
    }, 'diagnostics for the opened buffer');
    assert.strictEqual(diagnostics.length, 1);
    assert.strictEqual(diagnostics[0].severity, vscode.DiagnosticSeverity.Error);
    assert.strictEqual(diagnostics[0].source, 'jeti');
    assert.deepStrictEqual(diagnostics[0].range.start, { line: 0, character: 24 });
    assert.match(diagnostics[0].message, /expected ';'/);
    console.log(`ok  didOpen → collection has ${diagnostics.length} diagnostic: ${diagnostics[0].message}`);

    // didChange (full text) → cleared.
    fire('change', { document: documentFor(file, fixed) });
    await waitUntil(() => (collection().get(uri) || []).length === 0, 'cleared diagnostics');
    console.log('ok  didChange → collection cleared');

    // didClose → entry dropped from the collection.
    fire('close', documentFor(file, fixed));
    await waitUntil(() => !collection().has(uri), 'the closed document to be dropped');
    console.log('ok  didClose → entry dropped');

    // A non-jeti document is ignored (no crash, no publish).
    fire('open', {
      uri: { fsPath: '/tmp/other.txt', toString: () => 'file:///tmp/other.txt' },
      languageId: 'plaintext',
      getText: () => 'hi',
    });
    await new Promise((resolve) => setTimeout(resolve, 100));
    assert.ok(!collection().has('file:///tmp/other.txt'), 'non-jeti documents must not be sent');
    console.log('ok  non-jeti document ignored');

    // A syntax error and a type error on the *same* expression both reach VS
    // Code. The server reports both; the editor's hover shows one marker at a
    // time (the type error's range covers the missing `;`), but the collection -
    // what the Problems panel reads - carries the whole list.
    const nested = 'int main(void) {\n    [self release]\n    return 0;\n}\n';
    fire('open', documentFor(file, nested));
    const both = await waitUntil(() => {
      const entries = collection().get(uri);
      return entries && entries.length >= 2 ? entries : null;
    }, 'the syntax and the type diagnostic for one expression');
    const messages = both.map((diagnostic) => diagnostic.message);
    assert.ok(messages.some((m) => /expected ';'/.test(m)), `syntax error dropped: ${JSON.stringify(messages)}`);
    assert.ok(messages.some((m) => /ARC mode/.test(m)), `type error dropped: ${JSON.stringify(messages)}`);
    console.log(`ok  nested diagnostics → collection has ${both.length}: syntax + type`);
    fire('close', documentFor(file, nested));

    // Outline and definition go through the registered providers, convert to VS
    // Code types, and answer from the server's index.
    const program = [
      '#import <Foundation/Foundation.hje>',
      '',
      '@interface Greeter : NPObject',
      '- (void)greet:(NPString *)name from:(NPString *)where;',
      '@end',
      '',
      '@implementation Greeter',
      '- (void)greet:(NPString *)name from:(NPString *)where {',
      '    [self greet:name from:name];',
      '}',
      '@end',
      '',
      'int main(void) {',
      '    NPString *s = @"hi";',
      '    [s UTF8String];',
      '    return 0;',
      '}',
      '',
    ].join('\n');
    fire('open', documentFor(file, program));
    // Publishing is the same handler that stores the index, so waiting for the
    // publish means the outline/definition requests have something to answer.
    await waitUntil(() => collection().has(uri), 'the opened program to be published');
    await waitUntil(() => (collection().get(uri) || []).length === 0, 'a clean program');

    const symbolProvider = providers.get('documentSymbol');
    assert.ok(symbolProvider, 'expected a document symbol provider');
    const symbols = await symbolProvider.provideDocumentSymbols(documentFor(file, program));
    const names = symbols.map((symbol) => symbol.name);
    assert.deepStrictEqual(names, ['Greeter', 'Greeter', 'main'], JSON.stringify(symbols.map((s) => s.name)));
    assert.strictEqual(symbols[1].kind, vscode.SymbolKind.Class);
    assert.strictEqual(symbols[1].detail, '@implementation:jeti_root');
    assert.strictEqual(symbols[1].children.length, 1);
    assert.strictEqual(symbols[1].children[0].name, 'greet:from:');
    assert.strictEqual(symbols[1].children[0].kind, vscode.SymbolKind.Method);
    console.log('ok  documentSymbol provider → outline with methods');

    const definitionProvider = providers.get('definition');
    assert.ok(definitionProvider, 'expected a definition provider');
    const programDocument = documentFor(file, program);
    const local = await definitionProvider.provideDefinition(programDocument, positionOf(program, 'greet:name from'));
    assert.strictEqual(local.length, 2, JSON.stringify(local));
    assert.strictEqual(local[0].uri.toString(), uri);
    assert.strictEqual(local[0].range.start.line, positionOf(program, '- (void)greet:(NPString *)name from:(NPString *)where;').line);

    const imported = await definitionProvider.provideDefinition(programDocument, positionOf(program, 'NPString *s'));
    assert.strictEqual(imported.length, 1);
    assert.ok(
      imported[0].uri.toString().endsWith('include/Foundation/NPString.hje'),
      `expected a Foundation header, got ${imported[0].uri}`,
    );

    const missing = await definitionProvider.provideDefinition(programDocument, new vscode.Position(1, 0));
    assert.strictEqual(missing, undefined, 'a miss must stay a miss');
    console.log('ok  definition provider → local, imported header, and misses');

    // Hover renders the server's markdown as a MarkdownString; a symbol the
    // server knows nothing about yields no hover at all.
    const hoverProvider = providers.get('hover');
    assert.ok(hoverProvider, 'expected a hover provider');
    const hover = await hoverProvider.provideHover(programDocument, positionOf(program, 'NPString *s'));
    assert.ok(hover instanceof vscode.Hover);
    assert.ok(hover.contents.value.startsWith('**class** `NPString`'), hover.contents.value);
    assert.ok(hover.contents.value.includes('NPString.hje'), hover.contents.value);
    const noHover = await hoverProvider.provideHover(programDocument, new vscode.Position(1, 0));
    assert.strictEqual(noHover, undefined);
    console.log('ok  hover provider → markdown, and nothing for a non-symbol');

    // Completion carries the completion kind (Class = 7) and the detail line.
    const completionProvider = providers.get('completion');
    assert.ok(completionProvider, 'expected a completion provider');
    const prefixPosition = positionOf(program, 'NPString *s');
    prefixPosition.character += 3; // `NPS` typed
    const completions = await completionProvider.provideCompletionItems(programDocument, prefixPosition);
    assert.ok(completions.length > 0, 'expected completion items');
    assert.ok(completions.every((item) => item.label.startsWith('NPS')), JSON.stringify(completions.map((c) => c.label)));
    const npstring = completions.find((item) => item.label === 'NPString');
    assert.ok(npstring, `expected NPString in ${completions.map((c) => c.label)}`);
    assert.strictEqual(npstring.kind, vscode.CompletionItemKind.Class);
    assert.strictEqual(npstring.detail, '@interface:NPObject');
    console.log(`ok  completion provider → ${completions.length} item(s), kinds and details converted`);

    // Member completion: inside a send, the local's class answers, so `[s UTF8S`
    // offers only NPString's own member.
    const members = await completionProvider.provideCompletionItems(
      programDocument,
      positionOf(program, '[s UTF8S', 8),
    );
    assert.deepStrictEqual(members.map((item) => item.label), ['UTF8String']);
    assert.strictEqual(members[0].kind, vscode.CompletionItemKind.Method);
    console.log('ok  completion provider → receiver members inside a send');

    // Signature help: the parameter the cursor is filling in comes from the
    // server's argument ranges, and the label is shown as-is.
    const signatureProvider = providers.get('signatureHelp');
    assert.ok(signatureProvider, 'expected a signature help provider');
    const help = await signatureProvider.provideSignatureHelp(
      programDocument,
      positionOf(program, '[self greet:name from:name', 22),
    );
    assert.ok(help instanceof vscode.SignatureHelp, 'expected a SignatureHelp');
    assert.strictEqual(help.activeParameter, 1);
    assert.strictEqual(help.signatures.length, 1);
    assert.strictEqual(help.signatures[0].label, '- (void)greet:(NPString *)name from:(NPString *)where');
    assert.deepStrictEqual(help.signatures[0].parameters.map((p) => p.label), [
      '(NPString *)name',
      '(NPString *)where',
    ]);
    const noHelp = await signatureProvider.provideSignatureHelp(programDocument, new vscode.Position(1, 0));
    assert.strictEqual(noHelp, undefined, 'outside a call there is no signature');
    console.log('ok  signatureHelp provider → label, parameters, active parameter');

    // Full sync: the change event still carries the editor's ranges (the mock
    // applies them to the buffer first, as VS Code does), but the extension
    // forwards the whole buffer. Delete the `;` of `return 0;`, then put it back.
    const semicolon = positionOf(program, 'return 0;', 8);
    fire('change', {
      document: programDocument,
      contentChanges: [{ range: { start: semicolon, end: { line: semicolon.line, character: semicolon.character + 1 } }, text: '' }],
    });
    const introduced = await waitUntil(() => {
      const entries = collection().get(uri);
      return entries && entries.length ? entries : null;
    }, 'the range edit to be diagnosed');
    assert.match(introduced[0].message, /expected ';'/);
    assert.strictEqual(introduced[0].range.start.line, semicolon.line);

    fire('change', {
      document: programDocument,
      contentChanges: [{ range: { start: semicolon, end: semicolon }, text: ';' }],
    });
    await waitUntil(() => (collection().get(uri) || []).length === 0, 'the range edit to be undone');
    console.log('ok  didChange (full sync) → diagnostics track the edit');

    // Build switches reach the server: `jeti.arc: false` must spawn with
    // `-fno-jeti-arc`, which is what makes an MRC project stop showing ARC
    // errors (the analysis then matches the build).
    overrides.arc = false;
    overrides.arguments = ['-fno-checker'];
    outputLines.length = 0;
    fire('config', { affectsConfiguration: (section) => section === 'jeti' });
    await waitUntil(
      () => outputLines.some((line) => line.includes('jeti-lsp started')),
      'the restarted server with the project flags',
    );
    const started = outputLines.find((line) => line.includes('jeti-lsp started'));
    assert.ok(started.includes('-fno-jeti-arc'), started);
    assert.ok(started.includes('-fno-checker'), started);
    console.log('ok  jeti.arc / jeti.arguments → passed to the server');
    overrides.arc = true;
    overrides.arguments = [];

    // A missing server must not throw: it warns, stays inert, and a settings
    // change brings it back without reloading the window.
    await extension.deactivate();
    // A fresh extension host: no listeners left over from the first activate.
    listeners.clear();
    warnings.length = 0;
    overrides.serverPath = path.join(dir, 'no-such-server');
    await extension.activate({ subscriptions: [] });
    assert.ok(
      warnings.some((warning) => warning.includes('failed to start')),
      `expected a "failed to start" warning, got: ${warnings.join(' | ')}`,
    );
    console.log('ok  missing server → warning, no crash');

    outputLines.length = 0;
    overrides.serverPath = SERVER;
    fire('config', { affectsConfiguration: (section) => section === 'jeti' });
    await waitUntil(() => outputLines.some((line) => line.includes('jeti-lsp started')), 'the restarted server');
    fire('open', documentFor(file, broken));
    const revived = await waitUntil(() => {
      const entries = collection().get(uri);
      return entries && entries.length ? entries : null;
    }, 'diagnostics after the restart');
    assert.match(revived[0].message, /expected ';'/);
    console.log('ok  settings change → server revived and reporting again');
  } catch (error) {
    process.exitCode = 1;
    console.error(`FAIL: ${error.stack || error.message}`);
    if (outputLines.length) console.error(`extension log:\n${outputLines.join('\n')}`);
  } finally {
    await extension.deactivate();
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

let exited = false;
process.on('exit', () => {
  exited = true;
});
main().then(() => {
  if (!exited && process.exitCode !== 1) console.log('\nextension.test.js: all checks passed');
  if (!exited) process.exit(process.exitCode || 0); // the server's stdio keeps the loop alive
});
