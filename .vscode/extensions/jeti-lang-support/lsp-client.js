'use strict';

/**
 * Minimal LSP client for `jeti-lsp`.
 *
 * Only the transport and session bookkeeping live here — there is no `vscode`
 * import, so a plain node harness can drive the whole thing against the real
 * server binary (see `test/client.test.js`). `extension.js` is the thin VS Code
 * layer on top: it supplies the document events and renders
 * `textDocument/publishDiagnostics` into a DiagnosticCollection.
 *
 * Scope matches the server: the initialize handshake, full-document sync
 * (didOpen / didChange / didClose), pushed diagnostics, shutdown/exit. Adding
 * requests (completion, hover, definition) means adding request helpers here;
 * at that point it is worth reconsidering `vscode-languageclient`.
 *
 * Set `JETI_LSP_TRACE=1` to dump the raw protocol frames to stderr.
 */

const { spawn } = require('child_process');

const CONTENT_LENGTH = /^content-length:\s*(\d+)\s*$/i;

class JetiLanguageClient {
  /**
   * @param {object} options
   * @param {string} options.command   Server executable (absolute path or PATH name).
   * @param {string[]} [options.args]  Extra server arguments (`--include DIR`).
   * @param {string} [options.cwd]     Server working directory.
   * @param {(uri: string, diagnostics: object[]) => void} options.onDiagnostics
   *   Called for every `publishDiagnostics` (an empty array clears).
   * @param {(text: string) => void} [options.onLog]
   *   Server stderr and protocol-level notes.
   */
  constructor({ command, args = [], cwd, onDiagnostics, onLog }) {
    if (!command) throw new Error('jeti language client: no server command');
    if (typeof onDiagnostics !== 'function') {
      throw new Error('jeti language client: onDiagnostics is required');
    }
    this.command = command;
    this.args = args;
    this.cwd = cwd;
    this.onDiagnostics = onDiagnostics;
    this.onLog = onLog || (() => {});

    this.process = null;
    this.buffer = Buffer.alloc(0);
    this.nextId = 1;
    this.pending = new Map();
    this.versions = new Map();
    this.stopped = false;
  }

  /** Spawn the server and complete the `initialize` handshake. */
  async start({ rootUri = null, timeoutMs = 20000 } = {}) {
    if (this.process) throw new Error('jeti language client: already started');
    this.process = spawn(this.command, this.args, {
      cwd: this.cwd,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    this.process.stdout.on('data', (chunk) => this._receive(chunk));
    this.process.stderr.on('data', (chunk) => this.onLog(chunk.toString('utf8')));
    this.process.on('error', (error) => {
      this._failAll(error);
    });
    this.process.on('exit', (code, signal) => {
      this.stopped = true;
      this._failAll(new Error(`jeti-lsp exited (code=${code}, signal=${signal})`));
    });

    const result = await this._withTimeout(
      this.request('initialize', {
        processId: process.pid,
        rootUri,
        capabilities: {},
      }),
      timeoutMs,
      'initialize',
    );
    this.notify('initialized', {});
    return result;
  }

  /** `textDocument/didOpen`; full sync, so the whole text travels. */
  openDocument(uri, text, languageId = 'jeti') {
    const version = 1;
    this.versions.set(uri, version);
    this.notify('textDocument/didOpen', {
      textDocument: { uri, languageId, version, text },
    });
  }

  /**
   * `textDocument/didChange`.
   *
   * The server advertises `textDocumentSync: 1` (full), so the caller sends
   * the whole buffer as a single range-less change — a full replacement that
   * can never desync from the editor. Range-bearing changes are still
   * accepted by the server (for other clients) and travel unchanged.
   *
   * @param {string} uri
   * @param {Array<{range?: object, text: string}>} changes
   *   `[{ text: <whole buffer> }]` from this extension; ranges, when present,
   *   are 0-based lines with UTF-16 characters — LSP's own coordinates.
   */
  changeDocument(uri, changes) {
    const version = (this.versions.get(uri) || 1) + 1;
    this.versions.set(uri, version);
    const contentChanges = (changes || []).map((change) =>
      change.range ? { range: change.range, text: change.text } : { text: change.text });
    this.notify('textDocument/didChange', { textDocument: { uri, version }, contentChanges });
  }

  /** `textDocument/didClose`; the server then clears that document's diagnostics. */
  closeDocument(uri) {
    this.versions.delete(uri);
    this.notify('textDocument/didClose', { textDocument: { uri } });
  }

  /**
   * `textDocument/documentSymbol` → the outline (LSP `DocumentSymbol[]`).
   * Empty when the server could not analyze the buffer.
   *
   * @param {string} uri
   * @returns {Promise<object[]>}
   */
  documentSymbols(uri) {
    return this.request('textDocument/documentSymbol', { textDocument: { uri } }).then((symbols) => symbols || []);
  }

  /**
   * `textDocument/definition` → the declarations of the symbol at `position`,
   * or `null` when the server knows none (it answers a miss with `null` rather
   * than a guess).
   *
   * @param {string} uri
   * @param {{line: number, character: number}} position
   * @returns {Promise<object[] | null>}
   */
  definition(uri, position) {
    return this.request('textDocument/definition', { textDocument: { uri }, position });
  }

  /**
   * `textDocument/hover` → the markdown hover for the symbol at `position`, or
   * `null` when there is nothing to say about it.
   *
   * @param {string} uri
   * @param {{line: number, character: number}} position
   * @returns {Promise<object | null>}
   */
  hover(uri, position) {
    return this.request('textDocument/hover', { textDocument: { uri }, position });
  }

  /**
   * `textDocument/completion` → the completion list for `position`.
   *
   * @param {string} uri
   * @param {{line: number, character: number}} position
   * @returns {Promise<object[]>}
   */
  completion(uri, position) {
    return this.request('textDocument/completion', { textDocument: { uri }, position })
      .then((list) => list?.items || []);
  }

  /**
   * `textDocument/signatureHelp` → the signature of the call at `position`, or
   * `null` when the cursor is not inside a call this server can type.
   *
   * @param {string} uri
   * @param {{line: number, character: number}} position
   * @returns {Promise<object | null>}
   */
  signatureHelp(uri, position) {
    return this.request('textDocument/signatureHelp', { textDocument: { uri }, position });
  }

  /** `shutdown` + `exit`, then resolve with the server's exit code. */
  async stop({ timeoutMs = 10000 } = {}) {
    const proc = this.process;
    if (!proc) return 0;
    const exited = new Promise((resolve) => proc.once('exit', (code) => resolve(code)));
    try {
      await this._withTimeout(this.request('shutdown', null), timeoutMs, 'shutdown');
      this.notify('exit', null);
    } catch (error) {
      this.onLog(`jeti-lsp shutdown failed: ${error.message}\n`);
      proc.kill();
    }
    this.process = null;
    return this._withTimeout(exited, timeoutMs, 'exit').catch(() => {
      proc.kill();
      return null;
    });
  }

  /** Tear down without the graceful handshake (extension deactivate / errors). */
  kill() {
    this.process?.kill();
    this.process = null;
    this._failAll(new Error('jeti-lsp killed'));
  }

  request(method, params) {
    const id = this.nextId++;
    const promise = new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
    });
    this._send({ jsonrpc: '2.0', id, method, params });
    return promise;
  }

  notify(method, params) {
    this._send({ jsonrpc: '2.0', method, params });
  }

  // -- transport -----------------------------------------------------------

  _send(message) {
    if (!this.process) {
      throw new Error(`jeti language client: cannot send ${message.method}, server is not running`);
    }
    const body = Buffer.from(JSON.stringify(message), 'utf8');
    if (process.env.JETI_LSP_TRACE) {
      this.onLog(`>>> ${message.method || message.id} ${JSON.stringify(message.params ?? null)}\n`);
    }
    this.process.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
    this.process.stdin.write(body);
  }

  _receive(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    for (;;) {
      const headerEnd = this.buffer.indexOf('\r\n\r\n');
      if (headerEnd < 0) return;
      const headers = this.buffer.slice(0, headerEnd).toString('ascii');
      const length = this._contentLength(headers);
      if (length === null) {
        // Unframed garbage: drop the header block rather than spin forever.
        this.onLog(`jeti-lsp: framing error in headers ${JSON.stringify(headers)}\n`);
        this.buffer = this.buffer.slice(headerEnd + 4);
        continue;
      }
      const bodyStart = headerEnd + 4;
      if (this.buffer.length < bodyStart + length) return; // wait for the rest
      const body = this.buffer.slice(bodyStart, bodyStart + length).toString('utf8');
      this.buffer = this.buffer.slice(bodyStart + length);
      this._dispatch(body);
    }
  }

  _contentLength(headers) {
    for (const line of headers.split('\r\n')) {
      const match = CONTENT_LENGTH.exec(line);
      if (match) return Number(match[1]);
    }
    return null;
  }

  _dispatch(body) {
    let message;
    try {
      message = JSON.parse(body);
    } catch (error) {
      this.onLog(`jeti-lsp: unparseable message ${JSON.stringify(body)}\n`);
      return;
    }
    if (process.env.JETI_LSP_TRACE) {
      this.onLog(`<<< ${message.method || message.id} ${JSON.stringify(message.params ?? message.result ?? message.error ?? null)}\n`);
    }
    if (message.id !== undefined && (message.result !== undefined || message.error !== undefined)) {
      const waiter = this.pending.get(message.id);
      if (!waiter) return;
      this.pending.delete(message.id);
      if (message.error) {
        waiter.reject(new Error(`${message.error.code}: ${message.error.message}`));
      } else {
        waiter.resolve(message.result);
      }
      return;
    }
    if (message.method === 'textDocument/publishDiagnostics') {
      const { uri, diagnostics } = message.params || {};
      if (typeof uri === 'string') this.onDiagnostics(uri, diagnostics || []);
      return;
    }
    if (message.method === 'window/logMessage' || message.method === 'window/showMessage') {
      this.onLog(`${message.params?.type ?? ''} ${message.params?.message ?? ''}\n`);
    }
    // Server-initiated requests are not needed by this subset; anything else is
    // ignored on purpose rather than answered with a wrong reply.
  }

  _failAll(error) {
    for (const waiter of this.pending.values()) waiter.reject(error);
    this.pending.clear();
  }

  _withTimeout(promise, timeoutMs, what) {
    if (!timeoutMs) return promise;
    let timer;
    const timeout = new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`jeti-lsp: ${what} timed out after ${timeoutMs}ms`)), timeoutMs);
      timer.unref?.();
    });
    return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
  }
}

module.exports = { JetiLanguageClient };
