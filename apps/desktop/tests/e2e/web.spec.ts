import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';

import AxeBuilder from '@axe-core/playwright';
import type { Page } from '@playwright/test';

import type { WebObservation } from '../../src/lib/bridge-web';
import type { Action, LibraryEntry, Phase, Settings, Snapshot } from '../../src/lib/types';

import { expect, test } from './fixtures';
import { button, expectNoShift, press, status } from './helpers';
import {
  DEFAULT_WEB_CSP,
  makeDefaultSettings,
  makeDefaultSnapshot,
  startStandInServer,
} from './web-server.mjs';

const DESKTOP_DIR = process.cwd();
const RELEASE_DIST_DIR = resolve(DESKTOP_DIR, 'dist-web');
const FIXTURES_PATH = resolve(DESKTOP_DIR, '../../crates/scriber-core/tests/fixtures/bridge-shapes.json');

interface BridgeShapes {
  snapshot: Snapshot;
  snapshotFailed: Snapshot;
  snapshotPaused: Snapshot;
  snapshotPostProcessing: Snapshot;
  snapshotDownloading: Snapshot;
  libraryEntry: LibraryEntry;
  settings: Settings;
}

const SHAPES = JSON.parse(readFileSync(FIXTURES_PATH, 'utf8')) as BridgeShapes;

let testDistDir = RELEASE_DIST_DIR;

function listFilesRecursive(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      out.push(...listFilesRecursive(full));
    } else {
      out.push(full);
    }
  }
  return out;
}

function buildWebBundle(outDir: string, withSourceMaps: boolean): void {
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    VITE_SCRIBER_BACKEND: 'web',
    SCRIBER_WEB_SOURCEMAPS: withSourceMaps ? '1' : '0',
  };
  delete env.COVERAGE;
  execFileSync('pnpm', ['exec', 'vite', 'build', '--outDir', outDir], {
    cwd: DESKTOP_DIR,
    env,
    stdio: 'pipe',
  });
}

async function expectNoAxeViolations(page: Page): Promise<void> {
  const results = await new AxeBuilder({ page }).analyze();
  expect(results.violations.map((v) => `${v.id}: ${v.help} (${String(v.nodes.length)})`)).toEqual([]);
}

async function attachWebTestHook(
  page: Page,
  timeouts?: Partial<{
    helloMs: number;
    reconnectMs: number;
    rpcMs: number;
    drainMs: number;
    clipboardMs: number;
  }>,
): Promise<void> {
  await page.addInitScript((t) => {
    window.__scriberWebTest = {
      ...(t !== null ? { timeouts: t } : {}),
      observations: [],
    };
  }, timeouts ?? null);
}

async function readObservations(page: Page): Promise<WebObservation[]> {
  return page.evaluate(() => [...(window.__scriberWebTest?.observations ?? [])]);
}

async function installClipboardHarness(
  page: Page,
  mode: 'resolve' | 'early_reject' | 'late_reject' | 'hold' = 'resolve',
  rejectReason = 'clipboard denied',
): Promise<void> {
  await page.addInitScript(
    ([initialMode, initialReason]) => {
      interface ClipboardRecord {
        inGesture: boolean;
        text: string | null;
        outcome: 'pending' | 'resolved' | 'rejected';
        error: string | null;
      }
      interface ClipboardHarness {
        mode: string;
        reason: string;
        records: ClipboardRecord[];
        inGesture: boolean;
        pendingHolds: () => number;
        release: (index?: number) => void;
        failHeld: (reason: string, index?: number) => void;
      }
      const w = window as unknown as { __clipHarness?: ClipboardHarness };
      const heldResolvers: Array<{ resolve: () => void; reject: (err: Error) => void }> = [];
      const harness: ClipboardHarness = {
        mode: initialMode,
        reason: initialReason,
        records: [],
        inGesture: false,
        pendingHolds() {
          return heldResolvers.length;
        },
        release(index) {
          const item = index !== undefined ? heldResolvers[index] : heldResolvers.shift();
          item?.resolve();
        },
        failHeld(reason, index) {
          const item = index !== undefined ? heldResolvers[index] : heldResolvers.shift();
          item?.reject(new Error(reason));
        },
      };
      w.__clipHarness = harness;

      const enterGesture = (): void => {
        harness.inGesture = true;
        setTimeout(() => {
          harness.inGesture = false;
        }, 0);
      };
      const exitGesture = (): void => {
        harness.inGesture = false;
      };
      window.addEventListener('click', enterGesture, true);
      window.addEventListener('keydown', enterGesture, true);
      window.addEventListener('load', () => {
        window.addEventListener('click', exitGesture, false);
        window.addEventListener('keydown', exitGesture, false);
      });

      const clipboardApi = {
        write(items: ClipboardItem[]): Promise<void> {
          const rec: ClipboardRecord = {
            inGesture: harness.inGesture,
            text: null,
            outcome: 'pending',
            error: null,
          };
          harness.records.push(rec);

          if (harness.mode === 'early_reject') {
            rec.outcome = 'rejected';
            rec.error = harness.reason;
            return Promise.reject(new Error(harness.reason));
          }

          const first = items[0];
          if (first === undefined) {
            rec.outcome = 'rejected';
            rec.error = 'empty clipboard write';
            return Promise.reject(new Error('empty clipboard write'));
          }

          return first.getType('text/plain').then(
            async (blob) => {
              rec.text = await blob.text();
              if (harness.mode === 'late_reject') {
                rec.outcome = 'rejected';
                rec.error = harness.reason;
                throw new Error(harness.reason);
              }
              if (harness.mode === 'hold') {
                await new Promise<void>((resolve, reject) => {
                  heldResolvers.push({ resolve, reject });
                });
              }
              rec.outcome = 'resolved';
            },
            (err: unknown) => {
              rec.outcome = 'rejected';
              rec.error = err instanceof Error ? err.message : String(err);
              throw err;
            },
          );
        },
      };

      Object.defineProperty(navigator, 'clipboard', {
        configurable: true,
        value: clipboardApi,
      });
    },
    [mode, rejectReason] as const,
  );
}

async function openWeb(page: Page, url: string): Promise<void> {
  await page.goto(url);
  await expect(page.locator('html')).toHaveAttribute('data-bridge', 'web');
  await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
}

test.beforeAll(() => {
  if (process.env.COVERAGE) {
    testDistDir = resolve(DESKTOP_DIR, 'coverage/web-dist');
    mkdirSync(testDistDir, { recursive: true });
    buildWebBundle(testDistDir, true);
  } else {
    testDistDir = RELEASE_DIST_DIR;
    buildWebBundle(testDistDir, false);
  }
  process.env.SCRIBER_WEB_TEST_DIST = testDistDir;
});

test.afterAll(() => {
  if (testDistDir !== RELEASE_DIST_DIR) {
    rmSync(testDistDir, { recursive: true, force: true });
  }
});

// ---- AC 1: Release build verification ---------------------------------------

test('pnpm build:web emits dist-web with capture-worklet.js, no mock.ts, no __mockBridge and no test sourcemaps', async ({
  page,
}) => {
  const cleanEnv: NodeJS.ProcessEnv = { ...process.env };
  delete cleanEnv.COVERAGE;
  delete cleanEnv.SCRIBER_WEB_SOURCEMAPS;
  execFileSync('pnpm', ['run', 'build:web'], { cwd: DESKTOP_DIR, env: cleanEnv, stdio: 'pipe' });

  expect(existsSync(join(RELEASE_DIST_DIR, 'index.html'))).toBe(true);
  expect(existsSync(join(RELEASE_DIST_DIR, 'capture-worklet.js'))).toBe(true);

  const files = listFilesRecursive(RELEASE_DIST_DIR);
  const mapFiles = files.filter((f) => f.endsWith('.map'));
  expect(mapFiles).toEqual([]);

  const jsAssets = files.filter((f) => f.includes('/assets/') && f.endsWith('.js'));
  expect(jsAssets.length).toBeGreaterThan(0);
  for (const jsFile of jsAssets) {
    const text = readFileSync(jsFile, 'utf8');
    expect(text).not.toContain('__mockBridge');
    expect(text).not.toContain('0.0.0-mock');
  }

  const server = await startStandInServer({ distDir: RELEASE_DIST_DIR });
  try {
    await openWeb(page, server.url);
    expect(await page.evaluate(() => '__mockBridge' in window)).toBe(false);
    expect(server.connectionCount()).toBe(1);
    expect(server.totalConnections()).toBe(1);
  } finally {
    await server.close();
  }
});

// ---- AC 3, AC 4, AC 17: Auth, timeouts, reconnects, configureWebConnection, storage fallback, diagnostics sanitization

test('handles 4401 refusal without auto-retry, configureWebConnection storage + fallback, timeouts and zero secret leaks', async ({
  page,
}) => {
  const secretToken = 'SYNTHETIC_SECRET_TOKEN_98765';
  const server = await startStandInServer({
    distDir: testDistDir,
    token: secretToken,
  });

  try {
    await attachWebTestHook(page, { helloMs: 250, reconnectMs: 120, rpcMs: 300 });
    await openWeb(page, server.url);

    // Initial empty token is refused with 4401.
    await expect(page.locator('main').getByRole('alert')).toContainText('Authentication refused');
    const refusedText = (await page.locator('main').getByRole('alert').textContent()) ?? '';
    expect(refusedText).not.toContain(secretToken);

    // Wait past reconnectMs to verify NO automatic retry on 4401, and RPCs reject immediately while refused.
    await page.waitForTimeout(300);
    expect(server.totalConnections()).toBe(1);
    const refusedRpc = await page.evaluate(async () => {
      const b = window.__scriberWebTest?.bridge as {
        dictationState: () => Promise<Snapshot>;
        configureWebConnection: (cfg: { endpoint: string; token: string }) => void;
      };
      let badProto = '';
      try {
        b.configureWebConnection({ endpoint: 'ftp://example.invalid', token: '' });
      } catch (e) {
        badProto = e instanceof Error ? e.message : String(e);
      }
      try {
        await b.dictationState();
        return { badProto, err: null };
      } catch (e) {
        return { badProto, err: e instanceof Error ? e.message : String(e) };
      }
    });
    expect(refusedRpc.badProto).toContain('http, https, ws or wss');
    expect(refusedRpc.err).toContain('Authentication refused');

    const obsAfter4401 = await readObservations(page);
    expect(obsAfter4401.some((o) => o.category === 'connection' && o.event === 'hello' && o.outcome === 'refused' && o.reason === 'auth_4401')).toBe(true);
    expect(obsAfter4401.some((o) => o.category === 'connection' && o.event === 'retry' && o.outcome === 'skip' && o.reason === 'auth_refused')).toBe(true);

    // Explicit configureWebConnection stores normalized origin + token and rebinds immediately.
    await page.evaluate(
      ([endpoint, token]) => {
        const b = window.__scriberWebTest?.bridge as {
          configureWebConnection: (cfg: { endpoint: string; token: string }) => void;
        };
        b.configureWebConnection({ endpoint: `${endpoint}/ignored?secret=nope#frag`, token });
      },
      [server.url, secretToken] as const,
    );

    await expect(status(page)).toHaveText('Click New to start a dictation.');
    expect(await page.evaluate(() => localStorage.getItem('scriber.web.endpoint'))).toBe(server.url);
    expect(await page.evaluate(() => localStorage.getItem('scriber.web.token'))).toBe(secretToken);

    // Storage failure fallback: breaking localStorage still lets configureWebConnection work in memory.
    const nextToken = 'SYNTHETIC_FALLBACK_TOKEN_54321';
    server.setToken(nextToken);
    await page.evaluate(
      ([endpoint, token]) => {
        const throwing = () => {
          throw new DOMException('blocked', 'SecurityError');
        };
        Object.defineProperty(window, 'localStorage', {
          configurable: true,
          value: { getItem: throwing, setItem: throwing, removeItem: throwing },
        });
        const b = window.__scriberWebTest?.bridge as {
          configureWebConnection: (cfg: { endpoint: string; token: string }) => void;
        };
        b.configureWebConnection({ endpoint: endpoint.replace('http://', 'ws://'), token });
      },
      [server.url, nextToken] as const,
    );
    await expect(status(page)).toHaveText('Click New to start a dictation.');

    // Hello failure modes: invalid JSON, bad platform, close before hello, hello timeout, 1009 close.
    for (const mode of ['invalid_json', 'bad_platform', 'close_before_hello', 'hang'] as const) {
      server.setHelloMode(mode);
      server.disconnectAll(1001, 'test');
      await page.waitForTimeout(mode === 'hang' ? 350 : 200);
    }
    server.setHelloMode('ok');
    await expect(status(page)).toHaveText('Click New to start a dictation.');

    // 1009 close + RPC timeout + retirement rejects pending RPCs without replay.
    let stalledRpcCount = 0;
    server.setOnCall((ctx) => {
      if (ctx.call === 'ollama_models') {
        stalledRpcCount += 1;
        return true; // Intentionally never reply.
      }
      return false;
    });
    const timeoutErr = await page.evaluate(async () => {
      const b = window.__scriberWebTest?.bridge as { ollamaModels: () => Promise<string[]> };
      try {
        await b.ollamaModels();
        return null;
      } catch (e) {
        return e instanceof Error ? e.message : String(e);
      }
    });
    expect(timeoutErr).toContain('Timed out'.toLowerCase().replace('t', 'T').replace('Timed', 'timed'));
    expect(stalledRpcCount).toBe(1);

    // Pending RPC rejected on 1009 disconnect and NOT replayed on reconnect.
    const retireErrPromise = page.evaluate(async () => {
      const b = window.__scriberWebTest?.bridge as { ollamaModels: () => Promise<string[]> };
      try {
        await b.ollamaModels();
        return null;
      } catch (e) {
        return e instanceof Error ? e.message : String(e);
      }
    });
    await page.waitForTimeout(50);
    server.disconnectAll(1009, 'message too big');
    const retireErr = await retireErrPromise;
    expect(retireErr).toContain('server size limit');
    await expect(status(page)).toHaveText('Click New to start a dictation.');
    expect(stalledRpcCount).toBe(2); // Not replayed on reconnect!

    // Pre-hello RPC deadline timeout (rpcMs < helloMs) + cancelling pending reconnect timer via configureWebConnection + wss/https origin normalization.
    server.setOnCall(undefined);
    server.setHelloMode('hang');
    await page.evaluate(() => {
      if (window.__scriberWebTest !== undefined) {
        window.__scriberWebTest.timeouts = { helloMs: 600, reconnectMs: 120, rpcMs: 100 };
      }
    });
    server.disconnectAll(1001, 'hang next');
    await page.waitForTimeout(160);
    const preHelloTimeout = await page.evaluate(async () => {
      const b = window.__scriberWebTest?.bridge as { appVersion: () => Promise<string> };
      try {
        await b.appVersion();
        return null;
      } catch (e) {
        return e instanceof Error ? e.message : String(e);
      }
    });
    expect(preHelloTimeout).toContain('timed out');
    await page.evaluate(() => {
      if (window.__scriberWebTest !== undefined) {
        window.__scriberWebTest.timeouts = { helloMs: 250, reconnectMs: 120, rpcMs: 300 };
      }
    });

    // Exercise wss:// -> https:// -> wss:// origin path and immediate reconnect-timer cancellation.
    server.setHelloMode('ok');
    await page.evaluate(
      ([endpoint, token]) => {
        const b = window.__scriberWebTest?.bridge as {
          configureWebConnection: (cfg: { endpoint: string; token: string }) => void;
        };
        b.configureWebConnection({ endpoint: 'wss://127.0.0.1:1', token });
        b.configureWebConnection({ endpoint, token });
      },
      [server.url, nextToken] as const,
    );
    await expect(status(page)).toHaveText('Click New to start a dictation.');

    // Exercise bounded observation ring buffer cap (200 entries).
    await page.evaluate(async () => {
      const b = window.__scriberWebTest?.bridge as { appVersion: () => Promise<string> };
      for (let i = 0; i < 210; i++) await b.appVersion();
    });
    expect((await readObservations(page)).length).toBeLessThanOrEqual(200);

    // Sanitization check: neither synthetic token appears anywhere in observations or stand-in log.
    const allObs = JSON.stringify(await readObservations(page));
    const serverLog = JSON.stringify(server.log);
    expect(allObs).not.toContain(secretToken);
    expect(allObs).not.toContain(nextToken);
    expect(serverLog).not.toContain(secretToken);
    expect(serverLog).not.toContain(nextToken);
  } finally {
    await server.close();
  }
});

// ---- AC 5 & AC 6: Full call/arg/result matrix, events, out-of-order falsy ok, special cases

test('covers full protocol matrix against bridge-shapes.json, out-of-order falsy replies, events and special cases', async ({
  page,
}) => {
  const server = await startStandInServer({
    distDir: testDistDir,
    version: '0.12.0-matrix',
    initialSnapshot: SHAPES.snapshot,
    initialSettings: SHAPES.settings,
    initialLibrary: [{ entry: SHAPES.libraryEntry, transcript: 'Archived transcript body.' }],
  });

  try {
    await attachWebTestHook(page);
    await installClipboardHarness(page, 'resolve');
    await openWeb(page, server.url);

    // Out-of-order replies including falsy `ok` values (false, 0, "", null) + uncorrelated error.
    let releaseFirst: (() => void) | null = null;
    server.setOnCall((ctx) => {
      if (ctx.call === 'window_fullscreen' && releaseFirst === null) {
        releaseFirst = () => {
          ctx.replyOk(false);
        };
        return true;
      }
      if (ctx.call === 'library_get' && ctx.args.id === 'empty-entry') {
        ctx.replyOk('');
        return true;
      }
      return false;
    });

    const oooPromise = page.evaluate(async () => {
      const b = window.__scriberWebTest?.bridge as {
        windowFullscreen: () => Promise<boolean>;
        libraryGet: (id: string) => Promise<string>;
      };
      const order: string[] = [];
      const p1 = b.windowFullscreen().then((v) => {
        order.push('p1');
        return v;
      });
      const p2 = b.libraryGet('empty-entry').then((v) => {
        order.push('p2');
        return v;
      });
      const v2 = await p2;
      const v1 = await p1;
      return { v1, v2, order };
    });
    await expect.poll(() => releaseFirst !== null).toBe(true);
    const finishFirst = releaseFirst as (() => void) | null;
    finishFirst?.();
    const oooResult = await oooPromise;
    expect(oooResult).toEqual({ v1: false, v2: '', order: ['p2', 'p1'] });
    server.setOnCall(undefined);

    // Uncorrelated server error keeps socket open.
    server.sendRawToAll(JSON.stringify({ id: null, err: 'Malformed frame ignored' }));

    // Exercise every special case & full call/action/phase matrix.
    const matrix = await page.evaluate(
      async ([shapes]) => {
        const b = window.__scriberWebTest?.bridge as {
          appVersion: () => Promise<string>;
          dictationState: () => Promise<Snapshot>;
          act: (a: Action, text?: string) => Promise<Snapshot>;
          libraryList: () => Promise<LibraryEntry[]>;
          libraryGet: (id: string) => Promise<string>;
          settingsGet: () => Promise<Settings>;
          settingsSet: (s: Settings) => Promise<Settings>;
          ollamaModels: () => Promise<string[]>;
          inputDevices: () => Promise<{ defaultDevice: string | null; others: string[] }>;
          windowFullscreen: () => Promise<boolean>;
          onFullscreenChanged: (cb: (on: boolean) => void) => Promise<() => void>;
          onDictationChanged: (cb: (s: Snapshot) => void) => Promise<() => void>;
          onLevel: (cb: (lvl: number) => void) => Promise<() => void>;
          vocabularyOpen: () => Promise<void>;
          learnedVocabularyOpen: () => Promise<void>;
          pickVocabularyFile: (c: string) => Promise<string | null>;
          pickModelFolder: (c: string) => Promise<string | null>;
        };

        const version = await b.appVersion();
        const state = await b.dictationState();
        const devices = await b.inputDevices();
        const full = await b.windowFullscreen();
        const unsubFull = await b.onFullscreenChanged(() => undefined);
        unsubFull();
        unsubFull();

        const libList = await b.libraryList();
        const libText = await b.libraryGet(shapes.libraryEntry.id);
        const gotSettings = await b.settingsGet();
        const savedSettings = await b.settingsSet({ ...gotSettings, libraryWidth: 280 });
        const models = await b.ollamaModels();

        const rejected: Record<string, string> = {};
        for (const [name, fn] of [
          ['vocabulary_open', () => b.vocabularyOpen()],
          ['learned_vocabulary_open', () => b.learnedVocabularyOpen()],
          ['pick_vocabulary_file', () => b.pickVocabularyFile('/x')],
          ['pick_model_folder', () => b.pickModelFolder('/y')],
        ] as const) {
          try {
            await fn();
          } catch (e) {
            rejected[name] = e instanceof Error ? e.message : String(e);
          }
        }

        // Test all non-mic actions directly (mic actions exercised in capture suite).
        const acceptSnap = await b.act('accept', 'Edited accept.');
        const undoSnap = await b.act('undoLearn');
        const downloadSnap = await b.act('downloadModel');
        const copySnap = await b.act('copy', 'Edited copy.');

        const receivedSnaps: Phase[] = [];
        const receivedLevels: number[] = [];
        const unsubSnap = await b.onDictationChanged((s) => receivedSnaps.push(s.phase));
        const unsubLvl = await b.onLevel((l) => receivedLevels.push(l));

        return {
          version,
          state,
          devices,
          full,
          libList,
          libText,
          gotSettings,
          savedSettings,
          models,
          rejected,
          acceptPhase: acceptSnap.phase,
          undoLearned: undoSnap.learned,
          downloadModelError: downloadSnap.modelError,
          copyPhase: copySnap.phase,
          unsubSnapReady: typeof unsubSnap === 'function',
          unsubLvlReady: typeof unsubLvl === 'function',
        };
      },
      [SHAPES] as const,
    );

    expect(matrix.version).toBe('0.12.0-matrix');
    expect(matrix.state).toEqual(SHAPES.snapshot);
    expect(matrix.devices).toEqual({ defaultDevice: 'Browser microphone', others: [] });
    expect(matrix.full).toBe(true);
    expect(matrix.libList).toEqual([SHAPES.libraryEntry]);
    expect(matrix.libText).toBe('Archived transcript body.');
    expect(matrix.gotSettings).toEqual(SHAPES.settings);
    expect(matrix.savedSettings.libraryWidth).toBe(280);
    expect(matrix.models).toEqual(['gemma3:4b', 'qwen3.8:27b-mlx']);
    expect(matrix.rejected).toEqual({
      vocabulary_open: 'Not available in the web app.',
      learned_vocabulary_open: 'Not available in the web app.',
      pick_vocabulary_file: 'Not available in the web app.',
      pick_model_folder: 'Not available in the web app.',
    });
    expect(matrix.acceptPhase).toBe('finished');
    expect(matrix.undoLearned).toEqual([]);
    expect(matrix.downloadModelError).toBeNull();
    expect(matrix.copyPhase).toBe('finished');

    // Pin all 7 phases via events + level delivery + unsubscribe.
    const phaseCheck = await page.evaluate(async () => {
      const b = window.__scriberWebTest?.bridge as {
        onDictationChanged: (cb: (s: Snapshot) => void) => Promise<() => void>;
        onLevel: (cb: (l: number) => void) => Promise<() => void>;
      };
      const w = window as unknown as {
        __evPhases: Phase[];
        __evLevels: number[];
        __unsub: () => void;
      };
      w.__evPhases = [];
      w.__evLevels = [];
      const u1 = await b.onDictationChanged((s) => w.__evPhases.push(s.phase));
      const u2 = await b.onLevel((l) => w.__evLevels.push(l));
      w.__unsub = () => {
        u1();
        u2();
        u1();
        u2();
      };
      return true;
    });
    expect(phaseCheck).toBe(true);

    const allPhases: Phase[] = ['idle', 'ready', 'recording', 'paused', 'transcribing', 'finished', 'failed'];
    let rev = 100;
    for (const p of allPhases) {
      server.broadcastEvent('dictation-changed', { ...makeDefaultSnapshot(), revision: ++rev, phase: p });
    }
    server.broadcastEvent('dictation-level', 0.42);
    await page.waitForTimeout(80);
    await page.evaluate(() => {
      (window as unknown as { __unsub: () => void }).__unsub();
    });
    server.broadcastEvent('dictation-changed', { ...makeDefaultSnapshot(), revision: ++rev, phase: 'idle' });
    server.broadcastEvent('dictation-level', 0.99);
    await page.waitForTimeout(50);

    const evOut = await page.evaluate(() => {
      const w = window as unknown as { __evPhases: Phase[]; __evLevels: number[] };
      return { phases: w.__evPhases, levels: w.__evLevels };
    });
    expect(evOut.phases).toEqual(allPhases);
    expect(evOut.levels).toEqual([0.42]);

    // Verify every RPC ID logged was a positive uint32 and unique while in-flight.
    const rpcIds = server.log
      .filter((e): e is Extract<typeof e, { kind: 'rpc' }> => e.kind === 'rpc')
      .map((e) => e.id);
    expect(rpcIds.every((id) => typeof id === 'number' && Number.isInteger(id) && id >= 1 && id <= 0xffff_ffff)).toBe(true);
    const obs = await readObservations(page);
    expect(obs.some((o) => o.category === 'rpc' && o.event === 'uncorrelated' && o.outcome === 'failed')).toBe(true);
  } finally {
    await server.close();
  }
});

// ---- AC 7: Authenticated rebind, lower revision/libraryRevision reset, explicit retry after 4401

test('rebinds cleanly to a restarted server with lower revision and libraryRevision, and retries after 4401', async ({
  page,
}) => {
  const server1 = await startStandInServer({
    distDir: testDistDir,
    version: '1.0.0-old',
    initialSnapshot: {
      ...makeDefaultSnapshot(),
      revision: 25,
      phase: 'ready',
      libraryRevision: 7,
    },
    initialSettings: { ...makeDefaultSettings(), summaries: true },
    initialLibrary: [
      {
        entry: { id: '20261101-100000', title: '2026-11-01 10:00:00', summary: 'Old recording' },
        transcript: 'Old text.',
      },
    ],
  });
  const boundPort = server1.port;

  let server2: Awaited<ReturnType<typeof startStandInServer>> | null = null;
  try {
    await attachWebTestHook(page, { reconnectMs: 100 });
    await openWeb(page, server1.url);
    await expect(status(page)).toHaveText('Click Record and start talking.');
    await expect(page.getByText('Old recording')).toBeVisible();

    // Stop server1 and start server2 on the SAME port with LOWER revision (1) and libraryRevision (0).
    await server1.close();
    server2 = await startStandInServer({
      distDir: testDistDir,
      port: boundPort,
      version: '2.0.0-fresh',
      initialSnapshot: {
        ...makeDefaultSnapshot(),
        revision: 1,
        phase: 'idle',
        libraryRevision: 0,
      },
      initialSettings: { ...makeDefaultSettings(), summaries: false },
      initialLibrary: [
        {
          entry: { id: '20261122-090000', title: '2026-11-22 09:00:00', summary: 'Fresh recording' },
          transcript: 'Fresh text.',
        },
      ],
    });

    await expect(status(page)).toHaveText('Click New to start a dictation.');
    await expect(page.getByText('Fresh recording')).toBeVisible();

    // Open Settings to verify version and settings were refreshed.
    await page.getByRole('button', { name: 'Settings' }).click();
    await expect(page.getByText('2.0.0-fresh')).toBeVisible();
    await page.keyboard.press('Escape');

    // Normal revision ordering has resumed: stale revision 0 is ignored, revision 2 is accepted.
    server2.broadcastEvent('dictation-changed', { ...makeDefaultSnapshot(), revision: 0, phase: 'ready' });
    await page.waitForTimeout(50);
    await expect(status(page)).toHaveText('Click New to start a dictation.');
    server2.broadcastEvent('dictation-changed', { ...makeDefaultSnapshot(), revision: 2, phase: 'ready' });
    await expect(status(page)).toHaveText('Click Record and start talking.');
  } finally {
    await server2?.close();
  }

  // Explicit retry via Try again button after 4401 refusal, plus invalid stored endpoint and storage-read fallback on launch.
  const serverAuth = await startStandInServer({ distDir: testDistDir, token: 'need-secret' });
  try {
    await page.addInitScript(() => {
      const orig = window.localStorage.getItem.bind(window.localStorage);
      let first = true;
      Object.defineProperty(window, 'localStorage', {
        configurable: true,
        value: {
          getItem(k: string) {
            if (first && k === 'scriber.web.token') {
              first = false;
              throw new DOMException('blocked', 'SecurityError');
            }
            if (k === 'scriber.web.endpoint') return 'ftp://invalid.endpoint';
            return orig(k);
          },
          setItem: () => undefined,
          removeItem: () => undefined,
        },
      });
    });
    await attachWebTestHook(page, { reconnectMs: 100 });
    await openWeb(page, serverAuth.url);
    await expect(page.locator('main').getByRole('alert')).toContainText('Authentication refused');
    serverAuth.setToken('');
    await button(page, 'Try again').click();
    await expect(status(page)).toHaveText('Click New to start a dictation.');
  } finally {
    await serverAuth.close();
  }
});

// ---- AC 8, AC 9, AC 16: Real static worklet-to-wire audio under CSP, former-owner reconnect & recovery, capture failures

test('streams real static AudioWorklet PCM16 frames under strict CSP and recovers former-owner reconnect without mic restart', async ({
  page,
}) => {
  const server = await startStandInServer({
    distDir: testDistDir,
    csp: DEFAULT_WEB_CSP,
  });

  try {
    const cspViolations: string[] = [];
    page.on('console', (m) => {
      if (m.type() === 'error' && /Content Security Policy/i.test(m.text())) cspViolations.push(m.text());
    });
    await page.addInitScript(() => {
      document.addEventListener('securitypolicyviolation', (e) => {
        console.error(`Content Security Policy violation: ${e.violatedDirective} ${e.blockedURI}`);
      });
      const w = window as unknown as { __gumCalls: MediaStreamConstraints[] };
      w.__gumCalls = [];
      const orig = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
      navigator.mediaDevices.getUserMedia = (constraints?: MediaStreamConstraints) => {
        w.__gumCalls.push(constraints ?? {});
        return orig(constraints);
      };
    });
    await attachWebTestHook(page, { reconnectMs: 100 });
    await installClipboardHarness(page, 'resolve');
    await openWeb(page, server.url);

    // Record via click -> wait for real 8,000-byte frames from static /capture-worklet.js.
    await press(page, 'New', 'Record');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    await expect
      .poll(() => server.getAudioStats().acceptedFrames, { timeout: 5_000 })
      .toBeGreaterThanOrEqual(2);
    expect(server.getAudioStats().frameByteLengths[0]).toBe(8_000);

    // Pause drains and stops tracks; Resume via Space keydown restarts capture.
    await press(page, 'Pause');
    await expect(status(page)).toContainText('Paused');
    const afterPauseStats = server.getAudioStats();
    expect(afterPauseStats.acceptedBytes).toBeGreaterThan(0);

    await page.keyboard.press(' ');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    const framesBeforeResumeWait = server.getAudioStats().acceptedFrames;
    await expect
      .poll(() => server.getAudioStats().acceptedFrames, { timeout: 5_000 })
      .toBeGreaterThan(framesBeforeResumeWait);

    // Verify constraints on both Record (click) and Resume (keydown).
    const gumCalls = await page.evaluate(
      () => (window as unknown as { __gumCalls: MediaStreamConstraints[] }).__gumCalls,
    );
    expect(gumCalls).toHaveLength(2);
    for (const c of gumCalls) {
      expect(c).toEqual({
        audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
      });
    }

    // Former-owner disconnect while recording -> reconnect shows Connection lost notice and does NOT restart mic!
    server.disconnectAll(1001, 'network drop');
    await expect(status(page)).toHaveText('Connection lost. Press Done to finish or Discard to start over.');
    const gumAfterReconnect = await page.evaluate(
      () => (window as unknown as { __gumCalls: MediaStreamConstraints[] }).__gumCalls.length,
    );
    expect(gumAfterReconnect).toBe(2);

    // Recover via Done.
    await press(page, 'Done');
    await expect(status(page)).toHaveText('Copied to the clipboard.');

    // Second cycle: former-owner disconnect -> recover via Discard.
    await press(page, 'New', 'Record');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    server.disconnectAll(1001, 'network drop');
    await expect(status(page)).toHaveText('Connection lost. Press Done to finish or Discard to start over.');
    await press(page, 'Discard');
    await expect(status(page)).toHaveText('Click New to start a dictation.');

    // Another client's remote Record never triggers getUserMedia.
    const gumBeforeRemote = await page.evaluate(
      () => (window as unknown as { __gumCalls: MediaStreamConstraints[] }).__gumCalls.length,
    );
    server.broadcastEvent('dictation-changed', {
      ...server.getSnapshot(),
      revision: server.getSnapshot().revision + 1,
      phase: 'recording',
    });
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    const gumAfterRemote = await page.evaluate(
      () => (window as unknown as { __gumCalls: MediaStreamConstraints[] }).__gumCalls.length,
    );
    expect(gumAfterRemote).toBe(gumBeforeRemote);
    expect(cspViolations).toEqual([]);
  } finally {
    await server.close();
  }
});

// ---- AC 9 & AC 11: Deterministic drain totals, ordering, cleanup, drain timeout, denial/setup/reject/interruption

test('verifies deterministic ordered drain totals, drain timeout recovery, and capture failure/interruption teardown', async ({
  page,
}) => {
  const server = await startStandInServer({ distDir: testDistDir });

  try {
    await attachWebTestHook(page, { drainMs: 120, reconnectMs: 80 });
    await installClipboardHarness(page, 'resolve');

    // Deterministic audio harness: 48 kHz context, mock worklet port, track/node cleanup counters.
    await page.addInitScript(() => {
      interface FakeCaptureState {
        mode: 'ok' | 'deny' | 'setup_fail' | 'hold_gum' | 'drop_drain';
        gumInGesture: boolean[];
        ctxInGesture: boolean[];
        tracksStopped: number;
        nodesDisconnected: number;
        contextsClosed: number;
        inGesture: boolean;
        portOnMessage: ((ev: MessageEvent) => void) | null;
        resolveHeldGum: (() => void) | null;
        emitBlocks: (blockCount: number, blockLen: number) => void;
      }
      const w = window as unknown as { __cap: FakeCaptureState };
      const cap: FakeCaptureState = {
        mode: 'ok',
        gumInGesture: [],
        ctxInGesture: [],
        tracksStopped: 0,
        nodesDisconnected: 0,
        contextsClosed: 0,
        inGesture: false,
        portOnMessage: null,
        resolveHeldGum: null,
        emitBlocks(blockCount, blockLen) {
          for (let b = 0; b < blockCount; b++) {
            const arr = new Float32Array(blockLen);
            arr.fill(0.25);
            cap.portOnMessage?.(new MessageEvent('message', { data: { type: 'audio', samples: arr } }));
          }
        },
      };
      w.__cap = cap;

      const enterGesture = (): void => {
        cap.inGesture = true;
        setTimeout(() => {
          cap.inGesture = false;
        }, 0);
      };
      const exitGesture = (): void => {
        cap.inGesture = false;
      };
      window.addEventListener('click', enterGesture, true);
      window.addEventListener('keydown', enterGesture, true);
      window.addEventListener('load', () => {
        window.addEventListener('click', exitGesture, false);
        window.addEventListener('keydown', exitGesture, false);
      });

      const makeStream = (): MediaStream => {
        const track = {
          stop() {
            cap.tracksStopped += 1;
          },
        };
        return {
          getTracks: () => [track],
        } as unknown as MediaStream;
      };

      Object.defineProperty(navigator, 'mediaDevices', {
        configurable: true,
        value: {
          getUserMedia: () => {
            cap.gumInGesture.push(cap.inGesture);
            if (cap.mode === 'deny') {
              const err = new Error('Permission denied');
              err.name = 'NotAllowedError';
              return Promise.reject(err);
            }
            if (cap.mode === 'hold_gum') {
              return new Promise<MediaStream>((resolve) => {
                cap.resolveHeldGum = () => {
                  resolve(makeStream());
                };
              });
            }
            return Promise.resolve(makeStream());
          },
        },
      });

      class FakeAudioContext {
        sampleRate = 48_000;
        state = 'suspended';
        destination = {};
        audioWorklet = {
          addModule: () => {
            if (cap.mode === 'setup_fail') {
              return Promise.reject(new Error('worklet load failed'));
            }
            return Promise.resolve();
          },
        };
        constructor() {
          cap.ctxInGesture.push(cap.inGesture);
        }
        resume(): Promise<void> {
          this.state = 'running';
          return Promise.resolve();
        }
        createMediaStreamSource(): object {
          return {
            connect: () => undefined,
            disconnect: () => {
              cap.nodesDisconnected += 1;
            },
          };
        }
        createGain(): object {
          return {
            gain: { value: 1 },
            connect: () => undefined,
            disconnect: () => {
              cap.nodesDisconnected += 1;
            },
          };
        }
        close(): Promise<void> {
          this.state = 'closed';
          cap.contextsClosed += 1;
          return Promise.resolve();
        }
      }

      class FakeAudioWorkletNode {
        port = {
          set onmessage(fn: ((ev: MessageEvent) => void) | null) {
            cap.portOnMessage = fn;
          },
          postMessage(msg: { type: string; id?: number }) {
            if (msg.type === 'drain' && cap.mode !== 'drop_drain') {
              // Emit one final trailing block ahead of the drained ack to prove ordering.
              const tailBlock = new Float32Array(4_800); // 1,600 samples at 16 kHz = 3,200 bytes
              tailBlock.fill(0.5);
              cap.portOnMessage?.(new MessageEvent('message', { data: { type: 'audio', samples: tailBlock } }));
              cap.portOnMessage?.(new MessageEvent('message', { data: { type: 'drained', id: msg.id } }));
            }
          },
        };
        connect(): void {
          // no-op
        }
        disconnect(): void {
          cap.nodesDisconnected += 1;
        }
      }

      Object.defineProperty(window, 'AudioContext', { configurable: true, value: FakeAudioContext });
      Object.defineProperty(window, 'AudioWorkletNode', { configurable: true, value: FakeAudioWorkletNode });
    });

    await openWeb(page, server.url);
    await press(page, 'New');

    // 1. Permission denial -> no record RPC, cleanup done.
    await page.evaluate(() => {
      (window as unknown as { __cap: { mode: string } }).__cap.mode = 'deny';
    });
    await press(page, 'Record');
    await expect(page.getByRole('alert')).toContainText('Permission denied');
    expect(server.log.some((e) => e.kind === 'rpc' && e.action === 'record')).toBe(false);

    // 2. Setup failure -> tracks stopped, context closed, no record RPC.
    await page.evaluate(() => {
      (window as unknown as { __cap: { mode: string } }).__cap.mode = 'setup_fail';
    });
    await press(page, 'Record');
    await expect(page.getByRole('alert')).toContainText('worklet load failed');
    expect(server.log.some((e) => e.kind === 'rpc' && e.action === 'record')).toBe(false);

    // 3. Remote interruption during pending getUserMedia + late stream teardown.
    await page.evaluate(() => {
      (window as unknown as { __cap: { mode: string } }).__cap.mode = 'hold_gum';
    });
    await button(page, 'Record').click();
    await page.waitForTimeout(30);
    const tracksBeforeLate = await page.evaluate(
      () => (window as unknown as { __cap: { tracksStopped: number } }).__cap.tracksStopped,
    );
    server.broadcastEvent('dictation-changed', { ...server.getSnapshot(), revision: 9, phase: 'idle' });
    await page.waitForTimeout(30);
    await page.evaluate(() => {
      (window as unknown as { __cap: { resolveHeldGum: (() => void) | null } }).__cap.resolveHeldGum?.();
    });
    await page.waitForTimeout(30);
    const tracksAfterLate = await page.evaluate(
      () => (window as unknown as { __cap: { tracksStopped: number } }).__cap.tracksStopped,
    );
    expect(tracksAfterLate).toBe(tracksBeforeLate + 1);

    // Reset to ready and test rejected Record action + non-recording Record reply.
    server.setSnapshot({ ...makeDefaultSnapshot(), revision: 10, phase: 'ready' });
    server.broadcastEvent('dictation-changed', server.getSnapshot());
    await expect(status(page)).toHaveText('Click Record and start talking.');
    await page.evaluate(() => {
      (window as unknown as { __cap: { mode: string } }).__cap.mode = 'ok';
    });
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'record') {
        ctx.replyOk({ ...makeDefaultSnapshot(), revision: 10, phase: 'ready' });
        return true;
      }
      return false;
    });
    await press(page, 'Record');
    await expect(button(page, 'Record')).toBeEnabled();
    await expect(status(page)).toHaveText('Click Record and start talking.');

    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'record') {
        ctx.replyErr('Server refused record.');
        return true;
      }
      return false;
    });
    const tracksBeforeReject = await page.evaluate(
      () => (window as unknown as { __cap: { tracksStopped: number } }).__cap.tracksStopped,
    );
    await press(page, 'Record');
    await expect(page.getByRole('alert')).toContainText('Server refused record.');
    const tracksAfterReject = await page.evaluate(
      () => (window as unknown as { __cap: { tracksStopped: number } }).__cap.tracksStopped,
    );
    expect(tracksAfterReject).toBe(tracksBeforeReject + 1);
    server.setOnCall(undefined);

    // 4. Deterministic Pause drain: 48,000 samples (16,000 out = 4 full 8,000 B frames) + 4,800 tail (1,600 out = 3,200 B).
    server.resetAudioStats();
    await press(page, 'Record');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    await page.evaluate(() => {
      (window as unknown as { __cap: { emitBlocks: (n: number, l: number) => void } }).__cap.emitBlocks(4, 12_000);
    });
    await press(page, 'Pause');
    await expect(status(page)).toContainText('Paused');

    const pauseStats = server.getAudioStats();
    expect(pauseStats.acceptedFrames).toBe(5);
    expect(pauseStats.frameByteLengths).toEqual([8_000, 8_000, 8_000, 8_000, 3_200]);
    expect(pauseStats.acceptedSamples).toBe(17_600);
    expect(pauseStats.acceptedBytes).toBe(35_200);
    const pauseRpcIdx = server.orderedWireEvents.indexOf('rpc:dictation_act:pause');
    const lastPauseAudioIdx = server.orderedWireEvents.lastIndexOf('audio:3200');
    expect(lastPauseAudioIdx).toBeGreaterThan(-1);
    expect(lastPauseAudioIdx).toBeLessThan(pauseRpcIdx);

    // 5. Missing drain ack on Done -> recoverable failure with NO partial Done dispatch, then retry Done succeeds!
    server.resetAudioStats();
    await press(page, 'Resume');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    await page.evaluate(() => {
      const cap = (window as unknown as { __cap: { mode: string; emitBlocks: (n: number, l: number) => void } }).__cap;
      cap.emitBlocks(1, 12_000); // 1 full frame
      cap.mode = 'drop_drain';
    });
    await press(page, 'Done');
    await expect(page.getByRole('alert')).toContainText('Timed out waiting for microphone audio to finish flushing.');
    expect(server.orderedWireEvents.includes('rpc:dictation_act:done')).toBe(false);

    // Now capture is cleaned up; pressing Done again succeeds cleanly!
    await page.evaluate(() => {
      (window as unknown as { __cap: { mode: string } }).__cap.mode = 'ok';
    });
    await press(page, 'Done');
    await expect(status(page)).toHaveText('Copied to the clipboard.');

    // 6. Disconnect mid-drain cleans up pending drain waiter cleanly without sending Done.
    await press(page, 'New', 'Record');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    await page.evaluate(() => {
      (window as unknown as { __cap: { mode: string } }).__cap.mode = 'drop_drain';
    });
    server.resetAudioStats();
    await button(page, 'Pause').click();
    await page.waitForTimeout(30);
    server.disconnectAll(1001, 'mid-drain drop');
    await expect(status(page)).toHaveText('Connection lost. Press Done to finish or Discard to start over.');
    expect(server.orderedWireEvents.includes('rpc:dictation_act:pause')).toBe(false);
    await page.evaluate(() => {
      (window as unknown as { __cap: { mode: string } }).__cap.mode = 'ok';
    });
    // Pause while capture is NOT active exercises drain skip not_active.
    await press(page, 'Pause');
    await expect(status(page)).toContainText('Paused');

    // 7. Remote departure from recording tears down active capture cleanly.
    await press(page, 'Resume');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    const tracksBeforeRemoteStop = await page.evaluate(
      () => (window as unknown as { __cap: { tracksStopped: number } }).__cap.tracksStopped,
    );
    const pausedRemote: Snapshot = {
      ...server.getSnapshot(),
      revision: server.getSnapshot().revision + 1,
      phase: 'paused',
    };
    server.setSnapshot(pausedRemote);
    server.broadcastEvent('dictation-changed', pausedRemote);
    await expect(status(page)).toContainText('Paused');
    const tracksAfterRemoteStop = await page.evaluate(
      () => (window as unknown as { __cap: { tracksStopped: number } }).__cap.tracksStopped,
    );
    expect(tracksAfterRemoteStop).toBe(tracksBeforeRemoteStop + 1);

    // 7b. Discard while actively recording immediately cleans up tracks/nodes/AudioContext without drain.
    await press(page, 'Resume');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    const tracksBeforeDiscard = await page.evaluate(
      () => (window as unknown as { __cap: { tracksStopped: number } }).__cap.tracksStopped,
    );
    await press(page, 'Discard');
    await expect(status(page)).toHaveText('Click New to start a dictation.');
    const tracksAfterDiscard = await page.evaluate(
      () => (window as unknown as { __cap: { tracksStopped: number } }).__cap.tracksStopped,
    );
    expect(tracksAfterDiscard).toBe(tracksBeforeDiscard + 1);
    await press(page, 'New');
    await expect(status(page)).toHaveText('Click Record and start talking.');

    // 8. Missing mediaDevices and throwing AudioContext fail gracefully before RPC.
    await page.evaluate(() => {
      function ThrowingAudioContext(): never {
        throw new Error('audio context blocked');
      }
      Object.defineProperty(window, 'AudioContext', {
        configurable: true,
        value: ThrowingAudioContext,
      });
    });
    await press(page, 'Record');
    await expect(page.getByRole('alert')).toContainText('audio context blocked');
    await page.evaluate(() => {
      Object.defineProperty(navigator, 'mediaDevices', { configurable: true, value: undefined });
    });
    await press(page, 'Record');
    await expect(page.getByRole('alert')).toContainText('browser media capture is unavailable');

    // Verify synchronous gesture invocation on getUserMedia & AudioContext.
    const gestureFlags = await page.evaluate(() => {
      const cap = (window as unknown as { __cap: { gumInGesture: boolean[]; ctxInGesture: boolean[] } }).__cap;
      return { gum: cap.gumInGesture, ctx: cap.ctxInGesture };
    });
    expect(gestureFlags.gum.length).toBeGreaterThan(0);
    expect(gestureFlags.gum.every(Boolean)).toBe(true);
    expect(gestureFlags.ctx.every(Boolean)).toBe(true);
  } finally {
    await server.close();
  }
});

// ---- AC 12, AC 13, AC 14: Done clipboard association matrix & feedback projection

test('enforces Done clipboard association rules, feedback projection, early/late rejection and Copy fallback + learning RPC error', async ({
  page,
}) => {
  const server = await startStandInServer({ distDir: testDistDir });

  try {
    await attachWebTestHook(page, { clipboardMs: 200, reconnectMs: 80 });
    await installClipboardHarness(page, 'resolve');
    await openWeb(page, server.url);

    // Helper to reset server to paused (so Done skips mic capture) with a known revision.
    const setPaused = (rev: number, libRev = 0): void => {
      const snap: Snapshot = {
        ...makeDefaultSnapshot(),
        revision: rev,
        phase: 'paused',
        segments: 1,
        libraryRevision: libRev,
      };
      server.setSnapshot(snap);
      server.broadcastEvent('dictation-changed', snap);
    };

    // Case 1: Immediate finished reply via Return keydown gesture, with archive suffix & pending write hiding copied prefix.
    await page.evaluate(() => {
      (window as unknown as { __clipHarness: { mode: string } }).__clipHarness.mode = 'hold';
    });
    setPaused(10, 0);
    await expect(status(page)).toContainText('Paused');
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        const fin: Snapshot = {
          ...makeDefaultSnapshot(),
          revision: 11,
          phase: 'finished',
          text: 'Immediate finish text.',
          rawText: 'Immediate finish text.',
          notice: 'Copied to the clipboard. Could not keep the exact audio of the parts: disk full.',
        };
        server.setSnapshot(fin);
        ctx.replyOk(fin);
        return true;
      }
      return false;
    });

    await page.keyboard.press('Enter');
    // Synthetic copied prefix is hidden while write is pending; archive suffix is preserved!
    await expect(status(page)).toHaveText('Could not keep the exact audio of the parts: disk full.');
    await expect
      .poll(() =>
        page.evaluate(() => (window as unknown as { __clipHarness: { pendingHolds: () => number } }).__clipHarness.pendingHolds()),
      )
      .toBe(1);
    // Release clipboard write -> copied prefix appears alongside archive suffix!
    await page.evaluate(() => {
      (window as unknown as { __clipHarness: { release: () => void } }).__clipHarness.release();
    });
    await expect(status(page)).toHaveText(
      'Copied to the clipboard. Could not keep the exact audio of the parts: disk full.',
    );
    // Equal/later snapshot of the same finish preserves local copied outcome!
    server.broadcastEvent('dictation-changed', {
      ...server.getSnapshot(),
      revision: 12,
      libraryRevision: 1,
      notice: 'Copied to the clipboard.',
    });
    await expect(status(page)).toHaveText(
      'Copied to the clipboard. Could not keep the exact audio of the parts: disk full.',
    );

    // Case 2: Reply-first transcribing -> duplicate -> atomic archive finish (+2 rev, +1 libRev).
    await page.evaluate(() => {
      (window as unknown as { __clipHarness: { mode: string } }).__clipHarness.mode = 'resolve';
    });
    setPaused(20, 1);
    await expect(status(page)).toContainText('Paused');
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        const trans: Snapshot = {
          ...makeDefaultSnapshot(),
          revision: 21,
          phase: 'transcribing',
          pending: 1,
          libraryRevision: 1,
        };
        server.setSnapshot(trans);
        ctx.replyOk(trans);
        setTimeout(() => {
          server.broadcastEvent('dictation-changed', trans); // identical duplicate tolerated
          const fin: Snapshot = {
            ...makeDefaultSnapshot(),
            revision: 23, // +2 with libraryRevision +1 (atomic archive)
            phase: 'finished',
            text: 'Reply-first archived text.',
            rawText: 'Reply-first archived text.',
            notice: 'Copied to the clipboard.',
            libraryRevision: 2,
          };
          server.setSnapshot(fin);
          server.broadcastEvent('dictation-changed', fin);
        }, 30);
        return true;
      }
      return false;
    });
    await press(page, 'Done');
    await expect(status(page)).toHaveText('Copied to the clipboard.');

    // Case 3: Event-first completion (transcribing + finished arrive BEFORE Done RPC reply).
    setPaused(30, 2);
    await expect(status(page)).toContainText('Paused');
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        const trans: Snapshot = {
          ...makeDefaultSnapshot(),
          revision: 31,
          phase: 'transcribing',
          pending: 1,
          libraryRevision: 2,
        };
        const fin: Snapshot = {
          ...makeDefaultSnapshot(),
          revision: 32,
          phase: 'finished',
          text: 'Event-first text.',
          rawText: 'Event-first text.',
          notice: 'Copied to the clipboard.',
          libraryRevision: 2,
        };
        server.setSnapshot(fin);
        server.broadcastEvent('dictation-changed', trans);
        server.broadcastEvent('dictation-changed', fin);
        setTimeout(() => {
          ctx.replyOk(trans);
        }, 25);
        return true;
      }
      return false;
    });
    await press(page, 'Done');
    await expect(status(page)).toHaveText('Copied to the clipboard.');

    // Case 4: Rejected Done with foreign finished broadcast -> NO clipboard write!
    setPaused(40, 2);
    await expect(status(page)).toContainText('Paused');
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        const foreign: Snapshot = {
          ...makeDefaultSnapshot(),
          revision: 41,
          phase: 'finished',
          text: 'Foreign broadcast text.',
          rawText: 'Foreign broadcast text.',
          notice: 'Copied to the clipboard.',
          libraryRevision: 2,
        };
        server.setSnapshot(foreign);
        server.broadcastEvent('dictation-changed', foreign);
        ctx.replyErr('Done rejected by server.');
        return true;
      }
      return false;
    });
    await press(page, 'Done');
    await expect(page.getByRole('alert')).toContainText('Done rejected by server.');

    // Case 5: Failure followed by another client's Retry -> NO auto clipboard write, Retry finish hides copied prefix!
    setPaused(50, 2);
    await expect(status(page)).toContainText('Paused');
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        const trans: Snapshot = { ...makeDefaultSnapshot(), revision: 51, phase: 'transcribing', pending: 1, libraryRevision: 2 };
        ctx.replyOk(trans);
        setTimeout(() => {
          server.broadcastEvent('dictation-changed', {
            ...makeDefaultSnapshot(),
            revision: 52,
            phase: 'failed',
            error: 'Transcription failed.',
            libraryRevision: 2,
          });
          server.broadcastEvent('dictation-changed', {
            ...makeDefaultSnapshot(),
            revision: 53,
            phase: 'transcribing',
            pending: 1,
            libraryRevision: 2,
          });
          server.broadcastEvent('dictation-changed', {
            ...makeDefaultSnapshot(),
            revision: 54,
            phase: 'finished',
            text: 'Remote retry text.',
            rawText: 'Remote retry text.',
            notice: 'Copied to the clipboard.',
            libraryRevision: 2,
          });
        }, 20);
        return true;
      }
      return false;
    });
    await press(page, 'Done');
    await expect(page.locator('#transcript')).toHaveValue('Remote retry text.');
    await expect(status(page)).toHaveText('The transcript is ready.');

    // Case 6: Ambiguity (revision gap) + contradictory duplicate + empty finish + disconnect + timeout.
    setPaused(60, 2);
    await expect(status(page)).toContainText('Paused');
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        ctx.replyOk({ ...makeDefaultSnapshot(), revision: 61, phase: 'transcribing', pending: 1, libraryRevision: 2 });
        setTimeout(() => {
          server.broadcastEvent('dictation-changed', {
            ...makeDefaultSnapshot(),
            revision: 64, // gap!
            phase: 'finished',
            text: 'Gap text.',
            rawText: 'Gap text.',
            notice: 'Copied to the clipboard.',
            libraryRevision: 2,
          });
        }, 20);
        return true;
      }
      return false;
    });
    await press(page, 'Done');
    await expect(page.locator('#transcript')).toHaveValue('Gap text.');
    await expect(status(page)).toHaveText('The transcript is ready.');

    // Contradictory duplicate revision.
    setPaused(70, 2);
    await expect(status(page)).toContainText('Paused');
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        ctx.replyOk({ ...makeDefaultSnapshot(), revision: 71, phase: 'transcribing', pending: 1, libraryRevision: 2 });
        setTimeout(() => {
          server.broadcastEvent('dictation-changed', {
            ...makeDefaultSnapshot(),
            revision: 71,
            phase: 'transcribing',
            pending: 2, // contradicts reply!
            libraryRevision: 2,
          });
          server.broadcastEvent('dictation-changed', {
            ...makeDefaultSnapshot(),
            revision: 72,
            phase: 'finished',
            text: 'Contradicted text.',
            rawText: 'Contradicted text.',
            notice: 'Copied to the clipboard.',
            libraryRevision: 2,
          });
        }, 20);
        return true;
      }
      return false;
    });
    await press(page, 'Done');
    await expect(page.locator('#transcript')).toHaveValue('Contradicted text.');
    await expect(status(page)).toHaveText('The transcript is ready.');

    // Empty finish preserves no-speech notice and does not write clipboard.
    setPaused(80, 2);
    await expect(status(page)).toContainText('Paused');
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        ctx.replyOk({
          ...makeDefaultSnapshot(),
          revision: 81,
          phase: 'finished',
          text: '',
          rawText: '',
          notice: 'No speech was heard.',
          libraryRevision: 2,
        });
        return true;
      }
      return false;
    });
    await press(page, 'Done');
    await expect(status(page)).toHaveText('No speech was heard.');

    // Empty async finish (transcribing -> finished with empty text) & superseding phase & disconnect during transcribing.
    setPaused(85, 2);
    await expect(status(page)).toContainText('Paused');
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        ctx.replyOk({ ...makeDefaultSnapshot(), revision: 86, phase: 'transcribing', pending: 1, libraryRevision: 2 });
        setTimeout(() => {
          server.broadcastEvent('dictation-changed', {
            ...makeDefaultSnapshot(),
            revision: 87,
            phase: 'finished',
            text: '',
            rawText: '',
            notice: 'No speech was heard.',
            libraryRevision: 2,
          });
        }, 20);
        return true;
      }
      return false;
    });
    await press(page, 'Done');
    await expect(status(page)).toHaveText('No speech was heard.');

    // Superseding phase (transcribing -> idle).
    setPaused(88, 2);
    await expect(status(page)).toContainText('Paused');
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        ctx.replyOk({ ...makeDefaultSnapshot(), revision: 89, phase: 'transcribing', pending: 1, libraryRevision: 2 });
        setTimeout(() => {
          server.broadcastEvent('dictation-changed', { ...makeDefaultSnapshot(), revision: 90, phase: 'idle', libraryRevision: 2 });
        }, 20);
        return true;
      }
      return false;
    });
    await press(page, 'Done');
    await expect(status(page)).toHaveText('Click New to start a dictation.');

    // Disconnect during transcribing cancels Done clipboard association cleanly.
    setPaused(92, 2);
    await expect(status(page)).toContainText('Paused');
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        const trans: Snapshot = { ...makeDefaultSnapshot(), revision: 93, phase: 'transcribing', pending: 1, libraryRevision: 2 };
        server.setSnapshot(trans);
        ctx.replyOk(trans);
        setTimeout(() => {
          server.disconnectAll(1001, 'transcribing drop');
        }, 20);
        return true;
      }
      return false;
    });
    await press(page, 'Done');
    await page.waitForTimeout(160);

    // Association timeout.
    setPaused(95, 2);
    await expect(status(page)).toContainText('Paused');
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        ctx.replyOk({ ...makeDefaultSnapshot(), revision: 96, phase: 'transcribing', pending: 1, libraryRevision: 2 });
        return true;
      }
      return false;
    });
    await press(page, 'Done');
    await page.waitForTimeout(260);
    const obsTimeout = await readObservations(page);
    expect(obsTimeout.some((o) => o.category === 'clipboard' && o.outcome === 'timeout' && o.reason === 'association_timeout')).toBe(true);

    // Early rejection & late rejection + Copy fallback with edited draft + learning RPC failure.
    setPaused(100, 2);
    await expect(status(page)).toContainText('Paused');
    await page.evaluate(() => {
      const h = (window as unknown as { __clipHarness: { mode: string; reason: string } }).__clipHarness;
      h.mode = 'early_reject';
      h.reason = 'early denied';
    });
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        const fin: Snapshot = {
          ...makeDefaultSnapshot(),
          revision: 101,
          phase: 'finished',
          text: 'Early reject text.',
          rawText: 'Early reject text.',
          notice: 'Copied to the clipboard.',
          libraryRevision: 2,
        };
        server.setSnapshot(fin);
        ctx.replyOk(fin);
        return true;
      }
      return false;
    });
    await press(page, 'Done');
    await expect(status(page)).toHaveText('Could not copy to the clipboard: early denied. Use Copy to try again.');

    // Late rejection.
    setPaused(110, 2);
    await expect(status(page)).toContainText('Paused');
    await page.evaluate(() => {
      const h = (window as unknown as { __clipHarness: { mode: string; reason: string } }).__clipHarness;
      h.mode = 'late_reject';
      h.reason = 'late denied';
    });
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        const fin: Snapshot = {
          ...makeDefaultSnapshot(),
          revision: 111,
          phase: 'finished',
          text: 'Late reject text.',
          rawText: 'Late reject text.',
          notice: 'Copied to the clipboard.',
          libraryRevision: 2,
        };
        server.setSnapshot(fin);
        ctx.replyOk(fin);
        return true;
      }
      return false;
    });
    await press(page, 'Done');
    await expect(status(page)).toHaveText('Could not copy to the clipboard: late denied. Use Copy to try again.');

    // Copy fallback with edited draft: fails first when clipboard rejects (NO copy RPC sent!).
    const copyCallsBefore = server.log.filter((e) => e.kind === 'rpc' && e.action === 'copy').length;
    const recordsBeforeCopyFail = await page.evaluate(
      () => (window as unknown as { __clipHarness: { records: unknown[] } }).__clipHarness.records.length,
    );
    await page.locator('#transcript').fill('Edited fallback draft.');
    await press(page, 'Copy');
    await expect
      .poll(() =>
        page.evaluate((idx) => {
          const r = (window as unknown as { __clipHarness: { records: Array<{ outcome: string }> } }).__clipHarness.records[idx];
          return r?.outcome ?? null;
        }, recordsBeforeCopyFail),
      )
      .toBe('rejected');
    await expect(status(page)).toHaveText('Could not copy to the clipboard: late denied. Use Copy to try again.');
    expect(server.log.filter((e) => e.kind === 'rpc' && e.action === 'copy').length).toBe(copyCallsBefore);

    // Copy succeeds in browser, learning RPC fails -> clipboard stays copied, alert shows learning failure separately!
    await page.evaluate(() => {
      (window as unknown as { __clipHarness: { mode: string } }).__clipHarness.mode = 'resolve';
    });
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'copy') {
        ctx.replyErr('Learning store unavailable.');
        return true;
      }
      return false;
    });
    await press(page, 'Copy');
    await expect(page.getByRole('alert')).toHaveText('Learning store unavailable.');
    await expect(status(page)).toHaveText('Copied to the clipboard.');

    // Copy succeeds in browser and learning RPC succeeds -> shows Learned feedback!
    server.setOnCall(undefined);
    await press(page, 'Copy');
    await expect(status(page)).toHaveText('Copied to the clipboard. Learned: EditedTerm.');

    // ClipboardItem unavailable and throwing constructor paths on Copy.
    await page.evaluate(() => {
      function BadClipboardItem(): never {
        throw new Error('ctor boom');
      }
      Object.defineProperty(window, 'ClipboardItem', { configurable: true, value: BadClipboardItem });
    });
    await press(page, 'Copy');
    await expect(status(page)).toHaveText(
      'Could not copy to the clipboard: ctor boom. Use Copy to try again. Learned: EditedTerm.',
    );

    await page.evaluate(() => {
      Object.defineProperty(window, 'ClipboardItem', { configurable: true, value: undefined });
    });
    await press(page, 'Copy');
    await expect(status(page)).toHaveText(
      'Could not copy to the clipboard: clipboard unavailable. Use Copy to try again. Learned: EditedTerm.',
    );

    // Done directly replying with failed phase cancels association cleanly.
    setPaused(120, 2);
    await expect(status(page)).toContainText('Paused');
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'done') {
        const failedSnap: Snapshot = {
          ...makeDefaultSnapshot(),
          revision: 121,
          phase: 'failed',
          error: 'Direct failure.',
          libraryRevision: 2,
        };
        server.setSnapshot(failedSnap);
        ctx.replyOk(failedSnap);
        return true;
      }
      return false;
    });
    await press(page, 'Done');
    await expect(page.getByRole('alert')).toHaveText('Direct failure.');

    // Assert ONLY locally anchored finishes (plus explicit Copy) wrote successfully; foreign/retry/gap/contradicted/empty never wrote!
    const writtenTexts = await page.evaluate(() => {
      const h = (window as unknown as {
        __clipHarness: { records: Array<{ inGesture: boolean; text: string | null; outcome: string }> };
      }).__clipHarness;
      return {
        allInGesture: h.records.every((r) => r.inGesture),
        succeeded: h.records.filter((r) => r.outcome === 'resolved').map((r) => r.text),
      };
    });
    expect(writtenTexts.allInGesture).toBe(true);
    expect(writtenTexts.succeeded).toEqual([
      'Immediate finish text.',
      'Reply-first archived text.',
      'Event-first text.',
      'Edited fallback draft.',
      'Edited fallback draft.',
    ]);
  } finally {
    await server.close();
  }
});

// ---- AC 15: libraryCopy via library_get in click gesture, absence of library_copy RPC

test('libraryCopy writes via library_get in click gesture, reports success only after write, and never calls library_copy', async ({
  page,
}) => {
  const entry: LibraryEntry = {
    id: '20261122-110000',
    title: '2026-11-22 11:00:00',
    summary: 'Library clipboard test',
  };
  const server = await startStandInServer({
    distDir: testDistDir,
    initialLibrary: [{ entry, transcript: 'Library entry transcript.' }],
  });

  try {
    await attachWebTestHook(page);
    await installClipboardHarness(page, 'hold');
    await openWeb(page, server.url);

    await page.locator('#library li').first().getByRole('button').click();
    await expect(page.locator('#entry-transcript')).toHaveValue('Library entry transcript.');

    // Click Copy while clipboard write is held -> success MUST wait until write resolves.
    await press(page, 'Copy');
    await expect
      .poll(() =>
        page.evaluate(() => (window as unknown as { __clipHarness: { pendingHolds: () => number } }).__clipHarness.pendingHolds()),
      )
      .toBe(1);
    await expect(page.locator('.entry-status')).toHaveText('');
    await expect(page.locator('button.copy.copied')).toHaveCount(0);

    await page.evaluate(() => {
      (window as unknown as { __clipHarness: { release: () => void } }).__clipHarness.release();
    });
    await expect(page.locator('.entry-status')).toHaveText('Copied to the clipboard.');
    await expect(page.locator('button.copy.copied')).toBeVisible();

    // Missing entry error on library_get.
    server.setLibrary([]);
    await page.evaluate(() => {
      (window as unknown as { __clipHarness: { mode: string } }).__clipHarness.mode = 'resolve';
    });
    await press(page, 'Copy');
    await expect(page.getByRole('alert')).toContainText('Recording not found');

    // Clipboard write failure on libraryCopy.
    server.setLibrary([{ entry, transcript: 'Library entry transcript.' }]);
    await page.evaluate(() => {
      const h = (window as unknown as { __clipHarness: { mode: string; reason: string } }).__clipHarness;
      h.mode = 'early_reject';
      h.reason = 'write blocked';
    });
    await press(page, 'Copy');
    await expect(page.getByRole('alert')).toHaveText(
      'Could not copy to the clipboard: write blocked. Use Copy to try again.',
    );

    // Zero library_copy RPCs ever sent; all clipboard writes started in click gesture.
    expect(server.log.some((e) => e.kind === 'rpc' && e.call === 'library_copy')).toBe(false);
    const inGesture = await page.evaluate(() => {
      const h = (window as unknown as { __clipHarness: { records: Array<{ inGesture: boolean }> } }).__clipHarness;
      return h.records.every((r) => r.inGesture);
    });
    expect(inGesture).toBe(true);
  } finally {
    await server.close();
  }
});

// ---- AC 18: No layout shift & axe accessibility in light and dark on affected web states

for (const colorScheme of ['light', 'dark'] as const) {
  test.describe(`web layout & axe (${colorScheme})`, () => {
    test.use({ colorScheme });

    test('affected web states have zero layout shift and zero axe violations', async ({ page }) => {
      const server = await startStandInServer({ distDir: testDistDir });
      try {
        await attachWebTestHook(page, { reconnectMs: 80 });
        await installClipboardHarness(page, 'early_reject', 'no permission');
        await openWeb(page, server.url);

        // Recording -> former-owner reconnect notice has no layout shift and passes axe.
        await press(page, 'New', 'Record');
        await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
        await expectNoShift(page, page.locator('.controls'), async () => {
          server.disconnectAll(1001, 'drop');
          await expect(status(page)).toHaveText(
            'Connection lost. Press Done to finish or Discard to start over.',
          );
        });
        await expectNoAxeViolations(page);

        // Recover via Done (clipboard fails early -> exact failure sentence in .status-row, passes axe).
        await press(page, 'Done');
        await expect(status(page)).toHaveText(
          'Could not copy to the clipboard: no permission. Use Copy to try again.',
        );
        await expectNoAxeViolations(page);

        // Copy fallback succeeds -> no layout shift on #transcript and passes axe.
        await page.evaluate(() => {
          (window as unknown as { __clipHarness: { mode: string } }).__clipHarness.mode = 'resolve';
        });
        await expectNoShift(page, page.locator('#transcript'), async () => {
          await press(page, 'Copy');
          await expect(status(page)).toHaveText('Copied to the clipboard.');
        });
        await expectNoAxeViolations(page);
      } finally {
        await server.close();
      }
    });
  });
}
