// Isolated localhost Node stand-in for `crates/scriber-server`, used by
// `tests/e2e/web.spec.ts`. Serves the built web bundle under a strict CSP
// (no 'unsafe-inline', 'unsafe-eval', or blob:) and implements the documented
// `/ws` protocol from `docs/design-web.md`.

import { readFileSync, statSync } from 'node:fs';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { extname, resolve, sep } from 'node:path';

/**
 * @typedef {import('../../src/lib/types').Action} Action
 * @typedef {import('../../src/lib/types').LibraryEntry} LibraryEntry
 * @typedef {import('../../src/lib/types').Settings} Settings
 * @typedef {import('../../src/lib/types').Snapshot} Snapshot
 */

/**
 * @typedef {object} WsSocketLike
 * @property {number} readyState
 * @property {(data: string | Uint8Array) => void} send
 * @property {(code?: number, reason?: string) => void} close
 * @property {() => void} terminate
 * @property {(event: string, listener: (...args: unknown[]) => void) => void} on
 */

/**
 * @typedef {object} WsServerLike
 * @property {Set<WsSocketLike>} clients
 * @property {(event: string, listener: (...args: unknown[]) => void) => void} on
 * @property {(cb?: () => void) => void} close
 */

/**
 * @typedef {{ WebSocketServer: new (opts: { server: import('node:http').Server; path: string }) => WsServerLike }} WsModule
 */

/**
 * @typedef {'ok' | 'hang' | 'invalid_json' | 'bad_platform' | 'close_before_hello'} HelloMode
 */

/**
 * @typedef {{
 *   kind: 'hello';
 *   connId: number;
 *   authMatch: boolean;
 *   closeCode?: number;
 * } | {
 *   kind: 'rpc';
 *   connId: number;
 *   id: number | null;
 *   call: string;
 *   action?: string;
 *   argKeys: string[];
 *   ok: boolean;
 * } | {
 *   kind: 'audio';
 *   connId: number;
 *   bytes: number;
 *   samples: number;
 *   accepted: boolean;
 *   reason: string;
 * }} StandInLogEntry
 */

/**
 * @typedef {{
 *   connId: number;
 *   ws: WsSocketLike;
 *   id: number;
 *   call: string;
 *   args: Record<string, unknown>;
 *   replyOk: (value: unknown) => void;
 *   replyErr: (message: string) => void;
 * }} RpcContext
 */

/**
 * @typedef {(ctx: RpcContext) => boolean | Promise<boolean>} RpcInterceptor
 */

/**
 * @typedef {object} StandInOptions
 * @property {string} distDir
 * @property {number} [port]
 * @property {string} [token]
 * @property {string} [version]
 * @property {string} [csp]
 * @property {HelloMode} [helloMode]
 * @property {Snapshot} [initialSnapshot]
 * @property {Settings} [initialSettings]
 * @property {Array<{ entry: LibraryEntry; transcript: string }>} [initialLibrary]
 * @property {string[]} [ollamaModels]
 * @property {RpcInterceptor} [onCall]
 */

const require = createRequire(import.meta.url);
/** @type {unknown} */
const rawWsModule = require('ws');
const { WebSocketServer } = /** @type {WsModule} */ (rawWsModule);

/** Default strict CSP: no inline scripts, no eval, no blob: worklets. */
export const DEFAULT_WEB_CSP =
  "default-src 'self'; script-src 'self'; style-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; connect-src 'self' ws: wss:";

/** @type {Record<string, string>} */
const MIME_TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.json': 'application/json; charset=utf-8',
  '.map': 'application/json; charset=utf-8',
};

const MAX_FRAME_BYTES = 1_048_576;

/** @returns {Snapshot} */
export function makeDefaultSnapshot() {
  return {
    revision: 1,
    phase: 'idle',
    recordedMs: 0,
    segments: 0,
    pending: 0,
    text: '',
    error: null,
    notice: null,
    modelError: null,
    download: null,
    libraryRevision: 0,
    microphone: null,
    learned: [],
    partial: '',
    rawText: null,
    postProcessing: false,
  };
}

/** @returns {Settings} */
export function makeDefaultSettings() {
  return {
    summaries: true,
    libraryOpen: true,
    vocabulary: '/server/vocabulary.txt',
    cleanup: false,
    summaryModel: 'gemma3:4b',
    cleanupModel: 'qwen3.8:27b-mlx',
    libraryWidth: 220,
    inputDevice: '',
    silenceSplitMs: 0,
    keepSegments: false,
    learnedVocabulary: '/server/learned.txt',
    modelFolder: '/server/models',
    toggleShortcut: 'Opt+Cmd+D',
    pauseShortcut: 'Opt+Cmd+P',
    copySound: true,
  };
}

/**
 * @param {unknown} value
 * @returns {value is Record<string, unknown>}
 */
function isRecord(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * @param {Buffer | ArrayBuffer | Buffer[]} raw
 * @returns {Buffer}
 */
function toBuffer(raw) {
  if (Buffer.isBuffer(raw)) return raw;
  if (Array.isArray(raw)) return Buffer.concat(raw);
  return Buffer.from(raw);
}

/**
 * Starts an isolated localhost HTTP static + WebSocket stand-in server.
 * @param {StandInOptions} options
 */
export async function startStandInServer(options) {
  const rootDir = resolve(options.distDir);
  let expectedToken = options.token ?? '';
  let serverVersion = options.version ?? '0.12.0-web-standin';
  /** @type {HelloMode} */
  let helloMode = options.helloMode ?? 'ok';
  const csp = options.csp ?? DEFAULT_WEB_CSP;

  /** @type {Snapshot} */
  let snapshot = options.initialSnapshot ? structuredClone(options.initialSnapshot) : makeDefaultSnapshot();
  /** @type {Settings} */
  let settings = options.initialSettings ? structuredClone(options.initialSettings) : makeDefaultSettings();
  /** @type {Array<{ entry: LibraryEntry; transcript: string }>} */
  let library = options.initialLibrary ? structuredClone(options.initialLibrary) : [];
  /** @type {string[]} */
  let models = options.ollamaModels ? [...options.ollamaModels] : ['gemma3:4b', 'qwen3.8:27b-mlx'];
  /** @type {RpcInterceptor | undefined} */
  let onCall = options.onCall;

  /** @type {StandInLogEntry[]} */
  const log = [];
  /** @type {string[]} */
  const orderedWireEvents = [];
  /** @type {number[]} */
  const acceptedFrameByteLengths = [];
  /** @type {Int16Array[]} */
  const acceptedSampleChunks = [];

  let acceptedFrames = 0;
  let acceptedSamples = 0;
  let acceptedBytes = 0;
  let droppedFrames = 0;
  let nextConnId = 0;
  /** @type {number | null} */
  let ownerConnId = null;

  /** @type {Map<number, { ws: WsSocketLike; authenticated: boolean }>} */
  const connections = new Map();

  /**
   * @param {WsSocketLike} ws
   * @param {unknown} payload
   */
  function sendJson(ws, payload) {
    if (ws.readyState === 1) {
      ws.send(JSON.stringify(payload));
    }
  }

  /**
   * @param {'dictation-changed' | 'dictation-level'} event
   * @param {unknown} payload
   */
  function broadcastEvent(event, payload) {
    for (const conn of connections.values()) {
      if (conn.authenticated) {
        sendJson(conn.ws, { event, payload });
      }
    }
  }

  /** @param {string} rawText */
  function sendRawToAll(rawText) {
    for (const conn of connections.values()) {
      if (conn.authenticated && conn.ws.readyState === 1) {
        conn.ws.send(rawText);
      }
    }
  }

  /**
   * @param {number} connId
   * @param {Action} action
   * @param {string} text
   * @returns {{ ok: Snapshot } | { err: string }}
   */
  function runDefaultAction(connId, action, text) {
    const phase = snapshot.phase;
    if (action === 'new') {
      if (phase !== 'idle' && phase !== 'finished' && phase !== 'failed') {
        return { err: `Cannot start a new session in phase ${phase}.` };
      }
      ownerConnId = null;
      snapshot = {
        ...makeDefaultSnapshot(),
        revision: snapshot.revision + 1,
        phase: 'ready',
        libraryRevision: snapshot.libraryRevision,
      };
      return { ok: snapshot };
    }
    if (action === 'record') {
      if (phase !== 'ready') return { err: `Cannot record in phase ${phase}.` };
      ownerConnId = connId;
      snapshot = {
        ...snapshot,
        revision: snapshot.revision + 1,
        phase: 'recording',
        microphone: 'Browser microphone',
        notice: null,
        error: null,
      };
      return { ok: snapshot };
    }
    if (action === 'pause') {
      if (phase !== 'recording') return { err: `Cannot pause in phase ${phase}.` };
      ownerConnId = null;
      snapshot = {
        ...snapshot,
        revision: snapshot.revision + 1,
        phase: 'paused',
        segments: snapshot.segments + 1,
        recordedMs: snapshot.recordedMs + 1000,
        notice: null,
      };
      return { ok: snapshot };
    }
    if (action === 'resume') {
      if (phase !== 'paused') return { err: `Cannot resume in phase ${phase}.` };
      ownerConnId = connId;
      snapshot = {
        ...snapshot,
        revision: snapshot.revision + 1,
        phase: 'recording',
        microphone: 'Browser microphone',
        notice: null,
      };
      return { ok: snapshot };
    }
    if (action === 'done') {
      if (phase !== 'recording' && phase !== 'paused') {
        return { err: `Cannot finish in phase ${phase}.` };
      }
      ownerConnId = null;
      const transcript = 'Hello from web stand-in.';
      snapshot = {
        ...snapshot,
        revision: snapshot.revision + 1,
        phase: 'finished',
        segments: phase === 'recording' ? snapshot.segments + 1 : snapshot.segments,
        pending: 0,
        text: transcript,
        rawText: transcript,
        notice: 'Copied to the clipboard.',
        error: null,
        postProcessing: false,
      };
      return { ok: snapshot };
    }
    if (action === 'discard') {
      if (phase === 'idle' || phase === 'transcribing') {
        return { err: `Cannot discard in phase ${phase}.` };
      }
      ownerConnId = null;
      snapshot = {
        ...makeDefaultSnapshot(),
        revision: snapshot.revision + 1,
        phase: 'idle',
        libraryRevision: snapshot.libraryRevision,
      };
      return { ok: snapshot };
    }
    if (action === 'retry') {
      if (phase !== 'failed') return { err: `Cannot retry in phase ${phase}.` };
      snapshot = {
        ...snapshot,
        revision: snapshot.revision + 1,
        phase: 'transcribing',
        pending: 1,
        error: null,
      };
      return { ok: snapshot };
    }
    if (action === 'copy') {
      if (phase !== 'finished') return { err: `Cannot copy in phase ${phase}.` };
      const learned = text !== '' && text !== snapshot.text ? ['EditedTerm'] : snapshot.learned;
      snapshot = {
        ...snapshot,
        revision: snapshot.revision + 1,
        notice: 'Copied to the clipboard.',
        learned,
      };
      return { ok: snapshot };
    }
    if (action === 'accept') {
      if (phase !== 'finished') return { err: `Cannot accept in phase ${phase}.` };
      snapshot = {
        ...snapshot,
        revision: snapshot.revision + 1,
        learned: text !== '' && text !== snapshot.text ? ['AcceptedTerm'] : snapshot.learned,
      };
      return { ok: snapshot };
    }
    if (action === 'downloadModel') {
      snapshot = {
        ...snapshot,
        revision: snapshot.revision + 1,
        modelError: null,
        download: null,
      };
      return { ok: snapshot };
    }
    if (phase !== 'finished') return { err: `Cannot undo in phase ${phase}.` };
    snapshot = {
      ...snapshot,
      revision: snapshot.revision + 1,
      learned: [],
    };
    return { ok: snapshot };
  }

  const httpServer = createServer((req, res) => {
    const reqUrl = new URL(req.url ?? '/', 'http://127.0.0.1');
    if (reqUrl.pathname === '/healthz') {
      res.writeHead(200, { 'Content-Type': 'text/plain; charset=utf-8' });
      res.end('ok');
      return;
    }

    const rel = reqUrl.pathname === '/' ? 'index.html' : reqUrl.pathname.replace(/^\/+/, '');
    const filePath = resolve(rootDir, rel);
    if (filePath !== rootDir && !filePath.startsWith(`${rootDir}${sep}`)) {
      res.writeHead(404);
      res.end('Not found');
      return;
    }
    try {
      const stat = statSync(filePath);
      if (!stat.isFile()) {
        res.writeHead(404);
        res.end('Not found');
        return;
      }
      const body = readFileSync(filePath);
      const ext = extname(filePath);
      /** @type {Record<string, string>} */
      const headers = {
        'Content-Type': MIME_TYPES[ext] ?? 'application/octet-stream',
        'Content-Security-Policy': csp,
      };
      res.writeHead(200, headers);
      res.end(body);
    } catch {
      res.writeHead(404);
      res.end('Not found');
    }
  });

  const wss = new WebSocketServer({ server: httpServer, path: '/ws' });

  wss.on('connection', (/** @type {unknown} */ rawWs) => {
    const ws = /** @type {WsSocketLike} */ (rawWs);
    const connId = ++nextConnId;
    const state = { ws, authenticated: false };
    connections.set(connId, state);

    ws.on('close', () => {
      connections.delete(connId);
      if (ownerConnId === connId) {
        ownerConnId = null;
      }
    });

    ws.on('message', (/** @type {unknown} */ rawData, /** @type {unknown} */ rawIsBinary) => {
      const isBinary = Boolean(rawIsBinary);
      const buf = toBuffer(/** @type {Buffer | ArrayBuffer | Buffer[]} */ (rawData));
      if (buf.byteLength > MAX_FRAME_BYTES) {
        ws.close(1009, 'message too big');
        return;
      }

      if (!state.authenticated) {
        if (isBinary) {
          log.push({ kind: 'hello', connId, authMatch: false, closeCode: 4401 });
          ws.close(4401, 'expected hello');
          return;
        }
        /** @type {unknown} */
        let parsed;
        try {
          parsed = JSON.parse(buf.toString('utf8'));
        } catch {
          log.push({ kind: 'hello', connId, authMatch: false, closeCode: 4401 });
          ws.close(4401, 'invalid hello');
          return;
        }
        const hello = isRecord(parsed) && isRecord(parsed.hello) ? parsed.hello : null;
        const token = hello !== null && typeof hello.token === 'string' ? hello.token : null;
        if (token === null) {
          log.push({ kind: 'hello', connId, authMatch: false, closeCode: 4401 });
          ws.close(4401, 'invalid hello');
          return;
        }
        const authMatch = token === expectedToken;
        if (helloMode === 'hang') {
          log.push({ kind: 'hello', connId, authMatch });
          return;
        }
        if (helloMode === 'close_before_hello') {
          log.push({ kind: 'hello', connId, authMatch, closeCode: 1000 });
          ws.close(1000, 'closing');
          return;
        }
        if (helloMode === 'invalid_json') {
          log.push({ kind: 'hello', connId, authMatch });
          ws.send('{not-json');
          return;
        }
        if (helloMode === 'bad_platform') {
          log.push({ kind: 'hello', connId, authMatch });
          sendJson(ws, { hello: { version: serverVersion, platform: 'desktop' } });
          return;
        }
        if (!authMatch) {
          log.push({ kind: 'hello', connId, authMatch: false, closeCode: 4401 });
          ws.close(4401, 'unauthorized');
          return;
        }
        state.authenticated = true;
        log.push({ kind: 'hello', connId, authMatch: true });
        sendJson(ws, { hello: { version: serverVersion, platform: 'web' } });
        sendJson(ws, { event: 'dictation-changed', payload: snapshot });
        return;
      }

      if (isBinary) {
        if (buf.byteLength % 2 !== 0) {
          log.push({ kind: 'audio', connId, bytes: buf.byteLength, samples: 0, accepted: false, reason: 'odd_bytes' });
          sendJson(ws, { id: null, err: 'Odd-length PCM16 binary frame.' });
          return;
        }
        const samples = buf.byteLength / 2;
        if (snapshot.phase !== 'recording') {
          droppedFrames += 1;
          log.push({ kind: 'audio', connId, bytes: buf.byteLength, samples, accepted: false, reason: 'out_of_phase' });
          orderedWireEvents.push(`audio:dropped:out_of_phase:${buf.byteLength}`);
          return;
        }
        if (ownerConnId !== connId) {
          droppedFrames += 1;
          log.push({ kind: 'audio', connId, bytes: buf.byteLength, samples, accepted: false, reason: 'non_owner' });
          orderedWireEvents.push(`audio:dropped:non_owner:${buf.byteLength}`);
          return;
        }
        acceptedFrames += 1;
        acceptedSamples += samples;
        acceptedBytes += buf.byteLength;
        acceptedFrameByteLengths.push(buf.byteLength);
        const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
        const pcm = new Int16Array(samples);
        for (let i = 0; i < samples; i++) pcm[i] = view.getInt16(i * 2, true);
        acceptedSampleChunks.push(pcm);
        log.push({ kind: 'audio', connId, bytes: buf.byteLength, samples, accepted: true, reason: 'ok' });
        orderedWireEvents.push(`audio:${buf.byteLength}`);
        return;
      }

      /** @type {unknown} */
      let req;
      try {
        req = JSON.parse(buf.toString('utf8'));
      } catch {
        sendJson(ws, { id: null, err: 'Malformed JSON frame.' });
        return;
      }
      if (!isRecord(req) || typeof req.id !== 'number' || typeof req.call !== 'string' || !isRecord(req.args)) {
        sendJson(ws, { id: null, err: 'Invalid RPC envelope.' });
        return;
      }
      const id = req.id;
      const call = req.call;
      const args = req.args;
      const argKeys = Object.keys(args).sort();
      const actionName = call === 'dictation_act' && typeof args.action === 'string' ? args.action : undefined;
      orderedWireEvents.push(actionName !== undefined ? `rpc:${call}:${actionName}` : `rpc:${call}`);

      /** @param {unknown} value */
      const replyOk = (value) => {
        log.push({
          kind: 'rpc',
          connId,
          id,
          call,
          ...(actionName !== undefined ? { action: actionName } : {}),
          argKeys,
          ok: true,
        });
        sendJson(ws, { id, ok: value });
      };
      /** @param {string} message */
      const replyErr = (message) => {
        log.push({
          kind: 'rpc',
          connId,
          id,
          call,
          ...(actionName !== undefined ? { action: actionName } : {}),
          argKeys,
          ok: false,
        });
        sendJson(ws, { id, err: message });
      };

      const dispatch = async () => {
        if (onCall !== undefined) {
          const handled = await onCall({ connId, ws, id, call, args, replyOk, replyErr });
          if (handled) return;
        }

        switch (call) {
          case 'app_version':
            replyOk(serverVersion);
            return;
          case 'dictation_state':
            replyOk(snapshot);
            return;
          case 'dictation_act': {
            const action = /** @type {Action} */ (args.action);
            const text = typeof args.text === 'string' ? args.text : '';
            const res = runDefaultAction(connId, action, text);
            if ('err' in res) {
              replyErr(res.err);
            } else {
              replyOk(res.ok);
              broadcastEvent('dictation-changed', res.ok);
            }
            return;
          }
          case 'library_list':
            replyOk(library.map((item) => item.entry));
            return;
          case 'library_get': {
            const entryId = typeof args.id === 'string' ? args.id : '';
            const found = library.find((item) => item.entry.id === entryId);
            if (found === undefined) {
              replyErr(`Recording not found: ${entryId}`);
            } else {
              replyOk(found.transcript);
            }
            return;
          }
          case 'library_copy':
            replyOk(null);
            return;
          case 'settings_get':
            replyOk(settings);
            return;
          case 'settings_set':
            if (!isRecord(args.settings)) {
              replyErr('Invalid settings payload.');
              return;
            }
            settings = /** @type {Settings} */ (/** @type {unknown} */ (structuredClone(args.settings)));
            replyOk(settings);
            return;
          case 'ollama_models':
            replyOk(models);
            return;
          case 'input_devices':
            replyOk({ default: 'Browser microphone', others: [] });
            return;
          case 'window_fullscreen':
            replyOk(true);
            return;
          case 'vocabulary_open':
          case 'learned_vocabulary_open':
          case 'pick_vocabulary_file':
          case 'pick_model_folder':
            replyErr('Not available in the web app.');
            return;
          default:
            replyErr(`Unknown call: ${call}`);
        }
      };

      void dispatch();
    });
  });

  await new Promise((/** @type {(value: void) => void} */ resolveListen) => {
    httpServer.listen(options.port ?? 0, '127.0.0.1', () => {
      resolveListen();
    });
  });

  const addr = httpServer.address();
  const port = typeof addr === 'object' && addr !== null ? addr.port : 0;

  return {
    port,
    url: `http://127.0.0.1:${port}`,
    log,
    orderedWireEvents,
    getAudioStats() {
      return {
        acceptedFrames,
        acceptedSamples,
        acceptedBytes,
        droppedFrames,
        frameByteLengths: [...acceptedFrameByteLengths],
        sampleChunks: [...acceptedSampleChunks],
      };
    },
    resetAudioStats() {
      acceptedFrames = 0;
      acceptedSamples = 0;
      acceptedBytes = 0;
      droppedFrames = 0;
      acceptedFrameByteLengths.length = 0;
      acceptedSampleChunks.length = 0;
      orderedWireEvents.length = 0;
    },
    connectionCount() {
      return connections.size;
    },
    totalConnections() {
      return nextConnId;
    },
    getSnapshot() {
      return structuredClone(snapshot);
    },
    /** @param {Snapshot} next */
    setSnapshot(next) {
      snapshot = structuredClone(next);
    },
    /** @param {Settings} next */
    setSettings(next) {
      settings = structuredClone(next);
    },
    /** @param {Array<{ entry: LibraryEntry; transcript: string }>} next */
    setLibrary(next) {
      library = structuredClone(next);
    },
    /** @param {string[]} next */
    setOllamaModels(next) {
      models = [...next];
    },
    /** @param {string} next */
    setToken(next) {
      expectedToken = next;
    },
    /** @param {string} next */
    setVersion(next) {
      serverVersion = next;
    },
    /** @param {HelloMode} next */
    setHelloMode(next) {
      helloMode = next;
    },
    /** @param {RpcInterceptor | undefined} next */
    setOnCall(next) {
      onCall = next;
    },
    /** @param {number | null} next */
    setOwnerConnId(next) {
      ownerConnId = next;
    },
    broadcastEvent,
    sendRawToAll,
    /**
     * @param {number} [code]
     * @param {string} [reason]
     */
    disconnectAll(code = 1001, reason = 'server disconnect') {
      for (const conn of connections.values()) {
        conn.ws.close(code, reason);
      }
    },
    async close() {
      for (const conn of connections.values()) {
        conn.ws.terminate();
      }
      connections.clear();
      await new Promise((/** @type {(value: void) => void} */ done) => {
        wss.close(() => {
          httpServer.close(() => {
            done();
          });
        });
      });
    },
  };
}
