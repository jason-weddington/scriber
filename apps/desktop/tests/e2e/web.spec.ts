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
  // Built web HTML disables pinch-zoom (`user-scalable=no`) for iPad standalone
  // PWA ergonomics (docs/design-web.md). Verify the exact single viewport tag
  // independently before exempting only axe's `meta-viewport` rule.
  const viewports = await page.locator('meta[name="viewport"]').evaluateAll((els) =>
    els.map((el) => el.getAttribute('content')),
  );
  expect(viewports).toEqual(['width=device-width, initial-scale=1.0, user-scalable=no']);
  const results = await new AxeBuilder({ page }).disableRules(['meta-viewport']).analyze();
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
  expect(files.filter((f) => f.endsWith('scowl-en-60.txt'))).toEqual([]);

  const jsAssets = files.filter((f) => f.includes('/assets/') && f.endsWith('.js'));
  expect(jsAssets.length).toBeGreaterThan(0);
  for (const jsFile of jsAssets) {
    const text = readFileSync(jsFile, 'utf8');
    expect(text).not.toContain('__mockBridge');
    expect(text).not.toContain('0.0.0-mock');
    expect(text).not.toContain('scowl-en-60.txt');
    expect(text).not.toContain('aah\\naardvark\\naardvarks');
    expect(text).not.toContain('aah\naardvark\naardvarks');
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

    // Initial empty token is refused with 4401 and opens the Connect dialog.
    const connectDialog = page.getByRole('dialog', { name: 'Connect' });
    await expect(connectDialog).toBeVisible();
    await expect(connectDialog.getByRole('alert')).toHaveText('Authentication refused.');
    const refusedText = (await connectDialog.getByRole('alert').textContent()) ?? '';
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

    // Submitting through Connect stores normalized origin + token, rebinds immediately and closes Connect.
    await connectDialog.getByLabel('Server URL').fill(`${server.url}/ignored?secret=nope#frag`);
    await connectDialog.getByLabel('Shared token').fill(secretToken);
    await connectDialog.getByRole('button', { name: 'Connect', exact: true }).click();

    await expect(connectDialog).toBeHidden();
    await expect(status(page)).toHaveText('Click New to start a dictation.');
    expect(await page.evaluate(() => localStorage.getItem('scriber.web.endpoint'))).toBe(server.url);
    expect(await page.evaluate(() => localStorage.getItem('scriber.web.token'))).toBe(secretToken);

    // Storage failure fallback: breaking localStorage still lets Connect work in memory and records fallback observation.
    const nextToken = 'SYNTHETIC_FALLBACK_TOKEN_54321';
    server.setToken(nextToken);
    await page.evaluate(() => {
      const throwing = () => {
        throw new DOMException('blocked', 'SecurityError');
      };
      Object.defineProperty(window, 'localStorage', {
        configurable: true,
        value: { getItem: throwing, setItem: throwing, removeItem: throwing },
      });
    });
    await button(page, 'Settings').click();
    await page.getByRole('dialog', { name: 'Settings' }).getByRole('button', { name: 'Connect', exact: true }).click();
    await expect(connectDialog).toBeVisible();
    await connectDialog.getByLabel('Server URL').fill(server.url.replace('http://', 'ws://'));
    await connectDialog.getByLabel('Shared token').fill(nextToken);
    await connectDialog.getByRole('button', { name: 'Connect', exact: true }).click();
    await expect(connectDialog).toBeHidden();
    await expect(page.getByRole('dialog', { name: 'Settings' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(status(page)).toHaveText('Click New to start a dictation.');
    expect(
      (await readObservations(page)).some(
        (o) => o.category === 'connection' && o.event === 'storage' && o.outcome === 'skip' && o.reason === 'memory_fallback',
      ),
    ).toBe(true);

    // Hello failure modes: invalid JSON, bad platform, close before hello, hello timeout, 1009 close.
    const countHelloObs = async (outcome: WebObservation['outcome'], reason: string): Promise<number> =>
      (await readObservations(page)).filter(
        (o) => o.category === 'connection' && o.event === 'hello' && o.outcome === outcome && o.reason === reason,
      ).length;

    const helloModes: Array<
      readonly ['invalid_json' | 'bad_platform' | 'close_before_hello' | 'hang', WebObservation['outcome'], string]
    > = [
      ['invalid_json', 'failed', 'invalid_hello'],
      ['bad_platform', 'failed', 'invalid_hello'],
      ['close_before_hello', 'failed', 'closed_before_hello'],
      ['hang', 'timeout', 'hello_timeout'],
    ];
    for (const [mode, outcome, reason] of helloModes) {
      const before = await countHelloObs(outcome, reason);
      server.setHelloMode(mode);
      server.disconnectAll(1001, 'test');
      await expect.poll(() => countHelloObs(outcome, reason)).toBeGreaterThan(before);
      const authBefore = await countHelloObs('ok', 'authenticated');
      server.setHelloMode('ok');
      await expect.poll(() => countHelloObs('ok', 'authenticated')).toBeGreaterThan(authBefore);
    }
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
    const authBefore1009 = await countHelloObs('ok', 'authenticated');
    const retireErrPromise = page.evaluate(async () => {
      const b = window.__scriberWebTest?.bridge as { ollamaModels: () => Promise<string[]> };
      try {
        await b.ollamaModels();
        return null;
      } catch (e) {
        return e instanceof Error ? e.message : String(e);
      }
    });
    await expect.poll(() => stalledRpcCount).toBe(2);
    server.disconnectAll(1009, 'message too big');
    const retireErr = await retireErrPromise;
    expect(retireErr).toContain('server size limit');
    await expect.poll(() => countHelloObs('ok', 'authenticated')).toBeGreaterThan(authBefore1009);
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
    const totalConnsBeforeHang = server.totalConnections();
    server.disconnectAll(1001, 'hang next');
    await expect.poll(() => server.totalConnections()).toBeGreaterThan(totalConnsBeforeHang);
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
    const authBeforeReconfig = await countHelloObs('ok', 'authenticated');
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
    await expect.poll(() => countHelloObs('ok', 'authenticated')).toBeGreaterThan(authBeforeReconfig);
    await expect(status(page)).toHaveText('Click New to start a dictation.');

    // Verify meter lifecycle and socket routing observations carry only allowlisted keys and zero sentinel tokens.
    await press(page, 'New', 'Record');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    server.broadcastEvent('dictation-level', 0.33);
    await page.evaluate(async () => {
      const b = window.__scriberWebTest?.bridge as { appVersion: () => Promise<string> };
      await b.appVersion();
    });
    await press(page, 'Discard');
    await expect(status(page)).toHaveText('Click New to start a dictation.');
    const meterCheckObs = await readObservations(page);
    expect(meterCheckObs.some((o) => o.category === 'capture' && o.event === 'meter' && o.source === 'local')).toBe(
      true,
    );
    expect(
      meterCheckObs.some(
        (o) =>
          o.category === 'capture' &&
          o.event === 'level' &&
          o.source === 'socket' &&
          o.reason === 'suppressed-local',
      ),
    ).toBe(true);
    const allowedObsKeys = new Set([
      'seq',
      'category',
      'event',
      'outcome',
      'reason',
      'generation',
      'trigger',
      'priorAuth',
      'failureCode',
      'requestId',
      'phase',
      'revision',
      'frames',
      'samples',
      'bytes',
      'source',
      'armed',
      'draining',
      'sampleRate',
      'windowSamples',
      'completedWindows',
    ]);
    for (const record of meterCheckObs) {
      for (const key of Object.keys(record)) {
        expect(allowedObsKeys.has(key), `unexpected observation key: ${key}`).toBe(true);
      }
    }
    const meterCheckJson = JSON.stringify(meterCheckObs);
    expect(meterCheckJson).not.toContain(secretToken);
    expect(meterCheckJson).not.toContain(nextToken);
    expect(meterCheckJson).not.toContain('0.33');

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
    const connectDialog = page.getByRole('dialog', { name: 'Connect' });
    await expect(connectDialog).toBeVisible();
    await expect(connectDialog.getByRole('alert')).toHaveText('Authentication refused.');
    serverAuth.setToken('');
    await connectDialog.getByRole('button', { name: 'Try again', exact: true }).click();
    await expect(connectDialog).toBeHidden();
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

    // Mirror real scriber-server's Engine::act order: emit dictation-changed (phase: recording)
    // BEFORE replying to Record/Resume so capture is never torn down by its own event.
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && (ctx.args.action === 'record' || ctx.args.action === 'resume')) {
        const next: Snapshot = {
          ...server.getSnapshot(),
          revision: server.getSnapshot().revision + 1,
          phase: 'recording',
          microphone: 'Browser microphone',
          notice: null,
          error: null,
        };
        server.setOwnerConnId(ctx.connId);
        server.setSnapshot(next);
        server.broadcastEvent('dictation-changed', next);
        setTimeout(() => {
          ctx.replyOk(next);
        }, 15);
        return true;
      }
      return false;
    });

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
    const framesBeforeSecondCycle = server.getAudioStats().acceptedFrames;
    await press(page, 'New', 'Record');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    await expect
      .poll(() => server.getAudioStats().acceptedFrames, { timeout: 5_000 })
      .toBeGreaterThan(framesBeforeSecondCycle);
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
        mode: 'ok' | 'deny' | 'setup_fail' | 'hold_gum' | 'drop_drain' | 'hold_drain';
        gumInGesture: boolean[];
        ctxInGesture: boolean[];
        tracksStopped: number;
        nodesDisconnected: number;
        contextsClosed: number;
        inGesture: boolean;
        portOnMessage: ((ev: MessageEvent) => void) | null;
        savedHandlers: Array<(ev: MessageEvent) => void>;
        resolveHeldGum: (() => void) | null;
        releaseHeldDrain: (() => void) | null;
        emitBlocks: (blockCount: number, blockLen: number, amplitude?: number) => void;
        emitDistinctWindows: (amplitudes: number[], windowLen: number, blocks: number) => void;
        invokeSavedHandler: (handlerIdx: number, blockLen: number, amplitude?: number) => void;
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
        savedHandlers: [],
        resolveHeldGum: null,
        releaseHeldDrain: null,
        emitBlocks(blockCount, blockLen, amplitude = 0.25) {
          for (let b = 0; b < blockCount; b++) {
            const arr = new Float32Array(blockLen);
            arr.fill(amplitude);
            cap.portOnMessage?.(new MessageEvent('message', { data: { type: 'audio', samples: arr } }));
          }
        },
        emitDistinctWindows(amplitudes, windowLen, blocks) {
          const windowsPerBlock = amplitudes.length / blocks;
          for (let b = 0; b < blocks; b++) {
            const arr = new Float32Array(windowsPerBlock * windowLen);
            for (let wIdx = 0; wIdx < windowsPerBlock; wIdx++) {
              const amp = amplitudes[b * windowsPerBlock + wIdx] ?? 0.25;
              arr.fill(amp, wIdx * windowLen, (wIdx + 1) * windowLen);
            }
            cap.portOnMessage?.(new MessageEvent('message', { data: { type: 'audio', samples: arr } }));
          }
        },
        invokeSavedHandler(handlerIdx, blockLen, amplitude = 0.5) {
          const arr = new Float32Array(blockLen);
          arr.fill(amplitude);
          cap.savedHandlers[handlerIdx]?.(new MessageEvent('message', { data: { type: 'audio', samples: arr } }));
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
            // Simulate iOS/iPadOS Safari interrupting AudioContext when getUserMedia switches AVAudioSession.
            this.state = 'interrupted';
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
            if (fn !== null) cap.savedHandlers.push(fn);
          },
          postMessage(msg: { type: string; id?: number }) {
            if (msg.type === 'drain' && cap.mode !== 'drop_drain') {
              // Emit one final trailing block ahead of the drained ack to prove ordering.
              const tailBlock = new Float32Array(4_800); // 1,600 samples at 16 kHz = 3,200 bytes
              tailBlock.fill(0.5);
              cap.portOnMessage?.(new MessageEvent('message', { data: { type: 'audio', samples: tailBlock } }));
              if (cap.mode === 'hold_drain') {
                cap.releaseHeldDrain = () => {
                  cap.releaseHeldDrain = null;
                  cap.portOnMessage?.(new MessageEvent('message', { data: { type: 'drained', id: msg.id } }));
                };
                return;
              }
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

    // Non-recording remote interruption while dispatching tears down before arm() runs.
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'record') {
        server.broadcastEvent('dictation-changed', { ...makeDefaultSnapshot(), revision: 11, phase: 'idle' });
        setTimeout(() => {
          ctx.replyOk({ ...makeDefaultSnapshot(), revision: 12, phase: 'recording' });
        }, 15);
        return true;
      }
      return false;
    });
    const tracksBeforeDispatchInterrupt = await page.evaluate(
      () => (window as unknown as { __cap: { tracksStopped: number } }).__cap.tracksStopped,
    );
    await press(page, 'Record');
    await expect
      .poll(() => page.evaluate(() => (window as unknown as { __cap: { tracksStopped: number } }).__cap.tracksStopped))
      .toBe(tracksBeforeDispatchInterrupt + 1);
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    server.setOnCall(undefined);
    server.setSnapshot({ ...makeDefaultSnapshot(), revision: 13, phase: 'ready' });
    server.broadcastEvent('dictation-changed', server.getSnapshot());
    await expect(status(page)).toHaveText('Click Record and start talking.');

    // 4. Deterministic local 50 ms metering + Pause drain: 48,000 samples (20 bars; 16,000 out = 4 full 8,000 B frames)
    //    + 4,800 tail (0 local bars; 1,600 out = 3,200 B), plus pre-arm/armed/in-drain/held-Pause echo checks & remote fallback.
    const fenceSocket = async (): Promise<void> => {
      await page.evaluate(async () => {
        const b = window.__scriberWebTest?.bridge as { appVersion: () => Promise<string> };
        await b.appVersion();
      });
    };
    const readLevels = async (): Promise<number[]> =>
      page.evaluate(() => [...(window as unknown as { __levels: number[] }).__levels]);

    await page.evaluate(async () => {
      const b = window.__scriberWebTest?.bridge as {
        onLevel: (cb: (l: number) => void) => Promise<() => void>;
      };
      const w = window as unknown as { __levels: number[] };
      w.__levels = [];
      await b.onLevel((l) => w.__levels.push(l));
    });

    server.resetAudioStats();
    let releaseRecord: (() => void) | null = null;
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'record') {
        const recSnap: Snapshot = {
          ...server.getSnapshot(),
          revision: server.getSnapshot().revision + 1,
          phase: 'recording',
          microphone: 'Browser microphone',
          notice: null,
          error: null,
        };
        releaseRecord = () => {
          server.setOwnerConnId(ctx.connId);
          server.setSnapshot(recSnap);
          ctx.replyOk(recSnap);
          server.broadcastEvent('dictation-changed', recSnap);
        };
        return true;
      }
      return false;
    });

    const armedCountBefore = (await readObservations(page)).filter(
      (o) => o.category === 'capture' && o.event === 'start' && o.reason === 'armed',
    ).length;
    await button(page, 'Record').click();
    await expect.poll(() => releaseRecord !== null).toBe(true);

    // Pre-arm samples while local Record reply is held emit ZERO WebBridge.onLevel readings.
    await page.evaluate(() => {
      (window as unknown as { __cap: { emitBlocks: (n: number, l: number) => void } }).__cap.emitBlocks(1, 4_800);
    });
    expect(await readLevels()).toEqual([]);

    const finishRecord = releaseRecord as (() => void) | null;
    finishRecord?.();
    server.setOnCall(undefined);
    await expect
      .poll(
        async () =>
          (await readObservations(page)).filter(
            (o) => o.category === 'capture' && o.event === 'start' && o.reason === 'armed',
          ).length,
      )
      .toBe(armedCountBefore + 1);
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    const wave = page.locator('.wave');
    await expect(wave).toBeVisible();
    await expect.poll(async () => Number(await wave.getAttribute('data-slots'))).toBeGreaterThanOrEqual(25);
    expect(await readLevels()).toEqual([]);
    await expect(wave).toHaveAttribute('data-count', '0');

    // 48,000 armed samples at 48 kHz with 20 distinct window amplitudes -> 20 ordered callbacks & 20 distinguishable .wave bars.
    const distinctAmplitudes = Array.from({ length: 20 }, (_, i) => 10 ** ((-44 + i * 1.8) / 20));
    await page.evaluate((amps) => {
      (
        window as unknown as {
          __cap: { emitDistinctWindows: (a: number[], win: number, blocks: number) => void };
        }
      ).__cap.emitDistinctWindows(amps, 2_400, 4);
    }, distinctAmplitudes);

    const localLevels = await readLevels();
    expect(localLevels).toHaveLength(20);
    for (let i = 1; i < localLevels.length; i++) {
      expect(localLevels[i] ?? 0).toBeGreaterThan(localLevels[i - 1] ?? 0);
    }
    await expect(wave).toHaveAttribute('data-count', '20');
    const barHeights = await wave.locator('.bar').evaluateAll((els) =>
      els.slice(-20).map((el) => el.getBoundingClientRect().height),
    );
    expect(barHeights).toHaveLength(20);
    for (let i = 0; i < barHeights.length; i++) {
      expect(barHeights[i] ?? 0).toBeGreaterThan(4);
      if (i > 0) expect(barHeights[i] ?? 0).toBeGreaterThan(barHeights[i - 1] ?? 0);
    }

    // Injected dictation-level echo while armed leaves onLevel and .wave unchanged, observed as suppressed-local.
    server.broadcastEvent('dictation-level', 0.99);
    await fenceSocket();
    expect(await readLevels()).toHaveLength(20);
    await expect(wave).toHaveAttribute('data-count', '20');
    expect(
      (await readObservations(page)).some(
        (o) =>
          o.category === 'capture' &&
          o.event === 'level' &&
          o.outcome === 'skip' &&
          o.reason === 'suppressed-local' &&
          o.source === 'socket' &&
          o.armed === true &&
          o.draining === false,
      ),
    ).toBe(true);

    // Hold drain ack and Pause RPC reply to verify drain block emits 0 local levels and both in-drain & post-teardown echoes are suppressed.
    await page.evaluate(() => {
      (window as unknown as { __cap: { mode: string } }).__cap.mode = 'hold_drain';
      if (window.__scriberWebTest?.timeouts !== undefined) {
        window.__scriberWebTest.timeouts.drainMs = 5_000;
      }
    });
    let releasePause: (() => void) | null = null;
    server.setOnCall((ctx) => {
      if (ctx.call === 'dictation_act' && ctx.args.action === 'pause') {
        const pausedSnap: Snapshot = {
          ...server.getSnapshot(),
          revision: server.getSnapshot().revision + 1,
          phase: 'paused',
          segments: server.getSnapshot().segments + 1,
          recordedMs: server.getSnapshot().recordedMs + 1000,
          notice: null,
        };
        releasePause = () => {
          server.setOwnerConnId(null);
          server.setSnapshot(pausedSnap);
          ctx.replyOk(pausedSnap);
          server.broadcastEvent('dictation-changed', pausedSnap);
        };
        return true;
      }
      return false;
    });

    await button(page, 'Pause').click();
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as { __cap: { releaseHeldDrain: (() => void) | null } }).__cap.releaseHeldDrain !== null,
        ),
      )
      .toBe(true);
    // Synchronous 4,800-sample drain block already ran during drain and emitted ZERO local readings.
    expect(await readLevels()).toHaveLength(20);
    await expect(wave).toHaveAttribute('data-count', '20');

    // In-drain socket echo is suppressed with draining: true.
    server.broadcastEvent('dictation-level', 0.88);
    await fenceSocket();
    expect(await readLevels()).toHaveLength(20);
    await expect(wave).toHaveAttribute('data-count', '20');
    expect(
      (await readObservations(page)).some(
        (o) =>
          o.category === 'capture' &&
          o.event === 'level' &&
          o.outcome === 'skip' &&
          o.reason === 'suppressed-local' &&
          o.source === 'socket' &&
          o.armed === true &&
          o.draining === true,
      ),
    ).toBe(true);

    // Release drain ack -> tail flushes, capture tears down, Pause RPC is held while accepted phase is still recording.
    await page.evaluate(() => {
      (window as unknown as { __cap: { releaseHeldDrain: (() => void) | null } }).__cap.releaseHeldDrain?.();
    });
    await expect.poll(() => releasePause !== null).toBe(true);

    // Post-teardown same-generation dictation-level echo while Pause RPC is held is suppressed!
    server.broadcastEvent('dictation-level', 0.77);
    await fenceSocket();
    expect(await readLevels()).toHaveLength(20);
    await expect(wave).toHaveAttribute('data-count', '20');
    expect(
      (await readObservations(page)).some(
        (o) =>
          o.category === 'capture' &&
          o.event === 'level' &&
          o.outcome === 'skip' &&
          o.reason === 'suppressed-local' &&
          o.source === 'socket' &&
          o.armed === false &&
          o.draining === false,
      ),
    ).toBe(true);

    const finishPause = releasePause as (() => void) | null;
    finishPause?.();
    server.setOnCall(undefined);
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

    // Restore normal drain mode & fast drain timeout after held-drain check.
    await page.evaluate(() => {
      (window as unknown as { __cap: { mode: string } }).__cap.mode = 'ok';
      if (window.__scriberWebTest?.timeouts !== undefined) {
        window.__scriberWebTest.timeouts.drainMs = 120;
      }
    });

    // 4b. Newer remote recording snapshot restores socket-driven waveform updates (1 callback + 1 rendered .wave entry).
    const remoteRecording: Snapshot = {
      ...server.getSnapshot(),
      revision: server.getSnapshot().revision + 1,
      phase: 'recording',
      microphone: 'Remote microphone',
    };
    server.setSnapshot(remoteRecording);
    server.broadcastEvent('dictation-changed', remoteRecording);
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');

    server.broadcastEvent('dictation-level', 0.65);
    await fenceSocket();
    const afterRemoteLevels = await readLevels();
    expect(afterRemoteLevels).toHaveLength(21);
    expect(afterRemoteLevels.at(-1)).toBe(0.65);
    await expect(wave).toHaveAttribute('data-count', '21');
    const lastRemoteBar = await wave.locator('.bar').last().evaluate((el) => ({
      level: (el as HTMLElement).style.getPropertyValue('--level').trim(),
      height: el.getBoundingClientRect().height,
    }));
    expect(lastRemoteBar.level).toBe('0.65');
    expect(lastRemoteBar.height).toBeGreaterThan(4);
    expect(
      (await readObservations(page)).some(
        (o) =>
          o.category === 'capture' &&
          o.event === 'level' &&
          o.outcome === 'ok' &&
          o.reason === 'forwarded-remote' &&
          o.source === 'socket' &&
          o.armed === false &&
          o.draining === false,
      ),
    ).toBe(true);

    // Return to paused for isolated partial-window Resume verification.
    const pausedAgain: Snapshot = {
      ...server.getSnapshot(),
      revision: server.getSnapshot().revision + 1,
      phase: 'paused',
    };
    server.setSnapshot(pausedAgain);
    server.broadcastEvent('dictation-changed', pausedAgain);
    await expect(status(page)).toContainText('Paused');

    // 4c. Partial prior window is discarded on teardown: resumed BrowserCapture requires 2,400 fresh samples at 48 kHz.
    await press(page, 'Resume');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    const levelsBeforePartial = (await readLevels()).length;
    await page.evaluate(() => {
      (window as unknown as { __cap: { emitBlocks: (n: number, l: number) => void } }).__cap.emitBlocks(1, 1_200);
    });
    expect(await readLevels()).toHaveLength(levelsBeforePartial);
    await press(page, 'Pause');
    await expect(status(page)).toContainText('Paused');

    await press(page, 'Resume');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    await page.evaluate(() => {
      (window as unknown as { __cap: { emitBlocks: (n: number, l: number) => void } }).__cap.emitBlocks(1, 2_399);
    });
    expect(await readLevels()).toHaveLength(levelsBeforePartial);
    await page.evaluate(() => {
      (window as unknown as { __cap: { emitBlocks: (n: number, l: number) => void } }).__cap.emitBlocks(1, 1);
    });
    expect(await readLevels()).toHaveLength(levelsBeforePartial + 1);
    await press(page, 'Pause');
    await expect(status(page)).toContainText('Paused');

    // 5. Missing drain ack on Done -> recoverable failure with NO partial Done dispatch, stale handler blocked,
    //    server dictation-level forwarded once while still recording, then retry Done succeeds!
    server.resetAudioStats();
    await press(page, 'Resume');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    const handlerIdxBeforeTimeout = await page.evaluate(
      () => (window as unknown as { __cap: { savedHandlers: unknown[] } }).__cap.savedHandlers.length - 1,
    );
    await page.evaluate(() => {
      const cap = (window as unknown as { __cap: { mode: string; emitBlocks: (n: number, l: number) => void } }).__cap;
      cap.emitBlocks(1, 12_000); // 1 full frame (5 local windows)
      cap.mode = 'drop_drain';
    });
    await press(page, 'Done');
    await expect(page.getByRole('alert')).toContainText('Timed out waiting for microphone audio to finish flushing.');
    expect(server.orderedWireEvents.includes('rpc:dictation_act:done')).toBe(false);

    // Saved stale worklet handler cannot emit through WebBridge.onLevel after drain timeout.
    const levelsAfterTimeout = (await readLevels()).length;
    await page.evaluate((idx) => {
      (
        window as unknown as { __cap: { invokeSavedHandler: (i: number, len: number) => void } }
      ).__cap.invokeSavedHandler(idx, 4_800);
    }, handlerIdxBeforeTimeout);
    expect(await readLevels()).toHaveLength(levelsAfterTimeout);

    // Server dictation-level is forwarded exactly once while phase remains recording after local drain timeout.
    server.broadcastEvent('dictation-level', 0.42);
    await fenceSocket();
    const levelsAfterTimeoutForward = await readLevels();
    expect(levelsAfterTimeoutForward).toHaveLength(levelsAfterTimeout + 1);
    expect(levelsAfterTimeoutForward.at(-1)).toBe(0.42);

    // Now capture is cleaned up; pressing Done again succeeds cleanly!
    await page.evaluate(() => {
      (window as unknown as { __cap: { mode: string } }).__cap.mode = 'ok';
    });
    await press(page, 'Done');
    await expect(status(page)).toHaveText('Copied to the clipboard.');

    // 6. Disconnect mid-drain cleans up pending drain waiter cleanly without sending Done, and saved stale handler cannot emit after rebind.
    await press(page, 'New', 'Record');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    const handlerIdxBeforeDrop = await page.evaluate(
      () => (window as unknown as { __cap: { savedHandlers: unknown[] } }).__cap.savedHandlers.length - 1,
    );
    await page.evaluate(() => {
      (window as unknown as { __cap: { mode: string } }).__cap.mode = 'drop_drain';
    });
    server.resetAudioStats();
    await button(page, 'Pause').click();
    await page.waitForTimeout(30);
    server.disconnectAll(1001, 'mid-drain drop');
    await expect(status(page)).toHaveText('Connection lost. Press Done to finish or Discard to start over.');
    expect(server.orderedWireEvents.includes('rpc:dictation_act:pause')).toBe(false);
    const levelsAfterRebind = (await readLevels()).length;
    await page.evaluate((idx) => {
      (
        window as unknown as { __cap: { invokeSavedHandler: (i: number, len: number) => void } }
      ).__cap.invokeSavedHandler(idx, 4_800);
    }, handlerIdxBeforeDrop);
    expect(await readLevels()).toHaveLength(levelsAfterRebind);
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

    // Verify bounded, sanitized meter lifecycle & socket routing observations using only allowlisted fields.
    const captureObs = await readObservations(page);
    expect(captureObs.length).toBeLessThanOrEqual(200);
    const allowedKeys = new Set([
      'seq',
      'category',
      'event',
      'outcome',
      'reason',
      'generation',
      'trigger',
      'priorAuth',
      'failureCode',
      'requestId',
      'phase',
      'revision',
      'frames',
      'samples',
      'bytes',
      'source',
      'armed',
      'draining',
      'sampleRate',
      'windowSamples',
      'completedWindows',
    ]);
    for (const record of captureObs) {
      for (const key of Object.keys(record)) {
        expect(allowedKeys.has(key), `unexpected observation key: ${key}`).toBe(true);
      }
    }
    expect(
      captureObs.some(
        (o) =>
          o.category === 'capture' &&
          o.event === 'meter' &&
          o.outcome === 'ok' &&
          o.reason === 'setup' &&
          o.source === 'local' &&
          o.sampleRate === 48_000 &&
          o.windowSamples === 2_400 &&
          o.completedWindows === 0,
      ),
    ).toBe(true);
    expect(
      captureObs.some(
        (o) =>
          o.category === 'capture' &&
          o.event === 'meter' &&
          o.outcome === 'ok' &&
          o.reason === 'drained' &&
          o.source === 'local' &&
          o.armed === true &&
          o.draining === true &&
          o.sampleRate === 48_000 &&
          o.windowSamples === 2_400 &&
          o.completedWindows === 20,
      ),
    ).toBe(true);
    expect(
      captureObs.some(
        (o) =>
          o.category === 'capture' &&
          o.event === 'meter' &&
          o.outcome === 'timeout' &&
          o.reason === 'drain_timeout' &&
          o.source === 'local',
      ),
    ).toBe(true);
    const meterAndLevelJson = JSON.stringify(
      captureObs.filter((o) => o.category === 'capture' && (o.event === 'meter' || o.event === 'level')),
    );
    for (const sentinelLevel of ['0.99', '0.88', '0.77', '0.65', '0.42']) {
      expect(meterAndLevelJson).not.toContain(sentinelLevel);
    }
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
    const authBeforeDrop = (await readObservations(page)).filter(
      (o) => o.category === 'connection' && o.event === 'hello' && o.outcome === 'ok',
    ).length;
    await press(page, 'Done');
    await expect
      .poll(
        async () =>
          (await readObservations(page)).filter(
            (o) => o.category === 'connection' && o.event === 'hello' && o.outcome === 'ok',
          ).length,
      )
      .toBeGreaterThan(authBeforeDrop);

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
    await expect
      .poll(async () =>
        (await readObservations(page)).some(
          (o) => o.category === 'clipboard' && o.outcome === 'timeout' && o.reason === 'association_timeout',
        ),
      )
      .toBe(true);

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

        // Web Settings dialog: whole-document axe + expectNoShift on save error + short-viewport reachability.
        server.setOnCall((ctx) => {
          if (ctx.call === 'settings_set') {
            ctx.replyErr('Disk full.');
            return true;
          }
          return false;
        });
        await button(page, 'Settings').click();
        const settingsDialog = page.getByRole('dialog', { name: 'Settings' });
        await expect(settingsDialog).toBeVisible();
        await expectNoAxeViolations(page);
        await expectNoShift(page, settingsDialog, async () => {
          await settingsDialog.getByRole('switch', { name: 'Enable post-processing with local AI (Ollama)' }).click();
          await expect(settingsDialog.getByRole('alert')).toHaveText('Disk full.');
        });
        await expectNoAxeViolations(page);

        // Open Connect dialog from Settings: whole-document axe + expectNoShift on validation error.
        await settingsDialog.getByRole('button', { name: 'Connect', exact: true }).click();
        const connectDialog = page.getByRole('dialog', { name: 'Connect' });
        await expect(connectDialog).toBeVisible();
        await expectNoAxeViolations(page);
        await expectNoShift(page, connectDialog, async () => {
          await connectDialog.getByLabel('Server URL').fill('ftp://example.invalid');
          await connectDialog.getByRole('button', { name: 'Connect', exact: true }).click();
          await expect(connectDialog.getByRole('alert')).toHaveText('Enter a valid server URL.');
        });
        await expectNoAxeViolations(page);

        // Short-viewport keyboard reachability and no horizontal overflow in both dialogs under strict CSP.
        await page.setViewportSize({ width: 520, height: 240 });
        await connectDialog.getByLabel('Server URL').focus();
        await expect(connectDialog.getByLabel('Server URL')).toBeFocused();
        for (const locator of [
          connectDialog.getByLabel('Shared token'),
          connectDialog.getByRole('button', { name: 'Connect', exact: true }),
          connectDialog.getByRole('button', { name: 'Cancel', exact: true }),
        ]) {
          await locator.scrollIntoViewIfNeeded();
          await expect(locator).toBeInViewport();
        }
        expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);

        await page.keyboard.press('Escape');
        await expect(connectDialog).toBeHidden();
        await expect(settingsDialog).toBeVisible();
        await expect(settingsDialog.getByRole('button', { name: 'Connect', exact: true })).toBeFocused();
        for (const locator of [
          settingsDialog.getByLabel('Split into parts at a pause'),
          settingsDialog.getByRole('button', { name: 'Connect', exact: true }),
          settingsDialog.getByText(/^Version /),
        ]) {
          await locator.scrollIntoViewIfNeeded();
          await expect(locator).toBeInViewport();
        }
        expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
      } finally {
        await server.close();
      }
    });
  });
}

// ---- Web 3 AC 1, AC 2, AC 3: PWA HTML, manifest, PNG icons (dimensions & maskable 40% circle), no service worker ----

test('built web HTML/manifest/icons satisfy PWA contract, desktop/mock HTML stays untouched, and no service worker registers', async ({
  page,
}) => {
  // Verify desktop build HTML retains original viewport and has no PWA install tags.
  const desktopTempDir = resolve(DESKTOP_DIR, 'coverage/desktop-html-check');
  mkdirSync(desktopTempDir, { recursive: true });
  try {
    const desktopEnv: NodeJS.ProcessEnv = { ...process.env };
    delete desktopEnv.VITE_SCRIBER_BACKEND;
    delete desktopEnv.SCRIBER_WEB_SOURCEMAPS;
    delete desktopEnv.COVERAGE;
    execFileSync('pnpm', ['exec', 'vite', 'build', '--outDir', desktopTempDir], {
      cwd: DESKTOP_DIR,
      env: desktopEnv,
      stdio: 'pipe',
    });
    const desktopHtml = readFileSync(join(desktopTempDir, 'index.html'), 'utf8');
    const desktopViewports = desktopHtml.match(/<meta\s+name="viewport"[^>]*>/g) ?? [];
    expect(desktopViewports).toEqual(['<meta name="viewport" content="width=device-width, initial-scale=1.0" />']);
    expect(desktopHtml).not.toContain('apple-mobile-web-app-capable');
    expect(desktopHtml).not.toContain('apple-mobile-web-app-title');
    expect(desktopHtml).not.toContain('apple-touch-icon');
    expect(desktopHtml).not.toContain('manifest.webmanifest');
  } finally {
    rmSync(desktopTempDir, { recursive: true, force: true });
  }

  // Also check live mock dev HTML (baseURL) keeps original viewport and no install tags.
  await page.goto('/');
  expect(
    await page.locator('meta[name="viewport"]').evaluateAll((els) => els.map((e) => e.getAttribute('content'))),
  ).toEqual(['width=device-width, initial-scale=1.0']);
  await expect(page.locator('meta[name="apple-mobile-web-app-capable"]')).toHaveCount(0);
  await expect(page.locator('link[rel="manifest"]')).toHaveCount(0);
  await expect(page.locator('link[rel="apple-touch-icon"]')).toHaveCount(0);

  // Built web HTML served from stand-in server.
  const server = await startStandInServer({ distDir: testDistDir });
  try {
    await openWeb(page, server.url);

    expect(
      await page.locator('meta[name="viewport"]').evaluateAll((els) => els.map((e) => e.getAttribute('content'))),
    ).toEqual(['width=device-width, initial-scale=1.0, user-scalable=no']);
    await expect(page.locator('meta[name="apple-mobile-web-app-capable"]')).toHaveAttribute('content', 'yes');
    await expect(page.locator('meta[name="apple-mobile-web-app-title"]')).toHaveAttribute('content', 'Scriber');
    await expect(page.locator('link[rel="apple-touch-icon"]')).toHaveAttribute('href', '/apple-touch-icon.png');
    await expect(page.locator('link[rel="manifest"]')).toHaveAttribute('href', '/manifest.webmanifest');

    // Fetch manifest and verify schema.
    const manifestRes = await page.request.get(`${server.url}/manifest.webmanifest`);
    expect(manifestRes.status()).toBe(200);
    expect(manifestRes.headers()['content-type']).toContain('application/manifest+json');
    const manifest = (await manifestRes.json()) as {
      name: string;
      short_name: string;
      start_url: string;
      scope: string;
      display: string;
      icons: Array<{ src: string; sizes: string; type: string; purpose: string }>;
    };
    expect(manifest).toEqual({
      name: 'Scriber',
      short_name: 'Scriber',
      start_url: '/',
      scope: '/',
      display: 'standalone',
      icons: [
        { src: '/icon-192.png', sizes: '192x192', type: 'image/png', purpose: 'any' },
        { src: '/icon-512.png', sizes: '512x512', type: 'image/png', purpose: 'any' },
        { src: '/icon-maskable-192.png', sizes: '192x192', type: 'image/png', purpose: 'maskable' },
        { src: '/icon-maskable-512.png', sizes: '512x512', type: 'image/png', purpose: 'maskable' },
      ],
    });

    // Fetch all five PNG icons, asserting 200, image/png MIME, PNG dimensions, and maskable safe-zone pixel inspection.
    const expectedIcons = [
      { path: '/icon-192.png', size: 192, maskable: false },
      { path: '/icon-512.png', size: 512, maskable: false },
      { path: '/icon-maskable-192.png', size: 192, maskable: true },
      { path: '/icon-maskable-512.png', size: 512, maskable: true },
      { path: '/apple-touch-icon.png', size: 180, maskable: false },
    ] as const;

    for (const icon of expectedIcons) {
      const res = await page.request.get(`${server.url}${icon.path}`);
      expect(res.status(), icon.path).toBe(200);
      expect(res.headers()['content-type'], icon.path).toContain('image/png');
      const buf = await res.body();
      expect(buf.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])), icon.path).toBe(true);
      const width = buf.readUInt32BE(16);
      const height = buf.readUInt32BE(20);
      expect({ width, height }, icon.path).toEqual({ width: icon.size, height: icon.size });

      if (icon.maskable) {
        const inspect = await page.evaluate(async (url) => {
          const img = new Image();
          img.src = url;
          await img.decode();
          const canvas = document.createElement('canvas');
          canvas.width = img.naturalWidth;
          canvas.height = img.naturalHeight;
          const ctx = canvas.getContext('2d');
          if (ctx === null) throw new Error('no 2d context');
          ctx.drawImage(img, 0, 0);
          const { data, width: w, height: h } = ctx.getImageData(0, 0, canvas.width, canvas.height);
          const bgR = data[0] ?? 0;
          const bgG = data[1] ?? 0;
          const bgB = data[2] ?? 0;
          let minAlpha = 255;
          let maxFgRadius = 0;
          let fgPixels = 0;
          const cx = (w - 1) / 2;
          const cy = (h - 1) / 2;
          for (let y = 0; y < h; y++) {
            for (let x = 0; x < w; x++) {
              const i = (y * w + x) * 4;
              const r = data[i] ?? 0;
              const g = data[i + 1] ?? 0;
              const b = data[i + 2] ?? 0;
              const a = data[i + 3] ?? 0;
              if (a < minAlpha) minAlpha = a;
              if (r !== bgR || g !== bgG || b !== bgB) {
                fgPixels += 1;
                const d = Math.hypot(x - cx, y - cy);
                if (d > maxFgRadius) maxFgRadius = d;
              }
            }
          }
          return { w, h, minAlpha, fgPixels, maxFgRadius };
        }, `${server.url}${icon.path}`);
        expect(inspect.minAlpha, `${icon.path} opaque`).toBe(255);
        expect(inspect.fgPixels, `${icon.path} has artwork`).toBeGreaterThan(0);
        expect(inspect.maxFgRadius, `${icon.path} within 40% circle`).toBeLessThanOrEqual(0.4 * inspect.w);
      }
    }

    // No service worker is registered.
    const swCount = await page.evaluate(async () => {
      if (!('serviceWorker' in navigator)) return 0;
      return (await navigator.serviceWorker.getRegistrations()).length;
    });
    expect(swCount).toBe(0);
  } finally {
    await server.close();
  }
});

// ---- Web 3 AC 4, AC 5: Web Settings hidden Mac-only controls, full Settings round-trip, browser mic indicator ----

test('Web Settings hides Mac-only controls, preserves shared settings + hidden fields on save, and recorder shows browser mic without picker', async ({
  page,
}) => {
  const initialSettings: Settings = {
    summaries: true,
    libraryOpen: true,
    vocabulary: '/mac/custom/vocab.txt',
    cleanup: false,
    summaryModel: 'gemma3:4b',
    cleanupModel: 'qwen3.8:27b-mlx',
    libraryWidth: 260,
    inputDevice: 'MacBook Pro Microphone',
    silenceSplitMs: 1500,
    keepSegments: true,
    learnedVocabulary: '/mac/custom/learned.txt',
    modelFolder: '/mac/custom/models',
    toggleShortcut: 'Opt+Cmd+K',
    pauseShortcut: 'Opt+Cmd+J',
    copySound: false,
  };
  let savedPayload: Settings | null = null;
  const server = await startStandInServer({
    distDir: testDistDir,
    version: '0.12.0-web3',
    initialSettings,
    onCall: (ctx) => {
      if (ctx.call === 'settings_set') {
        savedPayload = structuredClone(ctx.args.settings as Settings);
      }
      return false;
    },
  });

  try {
    await openWeb(page, server.url);
    await expect(status(page)).toHaveText('Click New to start a dictation.');

    // Recorder shows non-interactive Browser microphone indicator without a picker in ready, recording, and paused.
    await press(page, 'New');
    await expect(page.locator('.mic-indicator')).toHaveText('Browser microphone');
    await expect(page.locator('button.mic')).toHaveCount(0);
    await expect(page.locator('[aria-haspopup="menu"]')).toHaveCount(0);
    await expect(page.getByRole('menu')).toHaveCount(0);

    await press(page, 'Record');
    await expect(page.locator('.mic-indicator')).toHaveText('Browser microphone');
    await expect(page.locator('button.mic')).toHaveCount(0);

    await press(page, 'Pause');
    await expect(page.locator('.mic-indicator')).toHaveText('Browser microphone');
    await expect(page.locator('button.mic')).toHaveCount(0);
    await press(page, 'Discard');

    // Open Web Settings and verify Mac-only controls are hidden while shared controls + Connect + version remain.
    await button(page, 'Settings').click();
    const dialog = page.getByRole('dialog', { name: 'Settings' });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByRole('group', { name: 'Vocabulary file' })).toHaveCount(0);
    await expect(dialog.getByRole('group', { name: 'Learned vocabulary file' })).toHaveCount(0);
    await expect(dialog.getByRole('group', { name: 'Speech model folder' })).toHaveCount(0);
    await expect(dialog.getByRole('group', { name: 'Toggle dictation shortcut' })).toHaveCount(0);
    await expect(dialog.getByRole('group', { name: 'Pause shortcut' })).toHaveCount(0);
    await expect(dialog.getByRole('switch', { name: 'Play a sound when the transcript is copied' })).toHaveCount(0);
    await expect(dialog.getByRole('switch')).toHaveCount(2);

    await expect(dialog.getByLabel('Split into parts at a pause')).toHaveValue('1500');
    await expect(dialog.getByRole('switch', { name: 'Enable post-processing with local AI (Ollama)' })).not.toBeChecked();
    await expect(dialog.getByRole('switch', { name: 'Generate library titles with local AI (Ollama)' })).toBeChecked();
    await expect(dialog.getByRole('combobox', { name: 'Post-processing model' })).toHaveValue('qwen3.8:27b-mlx');
    await expect(dialog.getByRole('combobox', { name: 'Title model' })).toHaveValue('gemma3:4b');
    await expect(dialog.getByRole('button', { name: 'Connect', exact: true })).toBeVisible();
    await expect(dialog.getByText('Version 0.12.0-web3')).toBeVisible();

    // Shared setting round-trip preserves full Settings shape including every hidden Mac-only field.
    await dialog.getByLabel('Split into parts at a pause').selectOption('4000');
    await expect.poll(() => savedPayload).toEqual({
      ...initialSettings,
      silenceSplitMs: 4000,
    });
    await dialog.getByRole('switch', { name: 'Enable post-processing with local AI (Ollama)' }).click();
    await expect.poll(() => savedPayload).toEqual({
      ...initialSettings,
      silenceSplitMs: 4000,
      cleanup: true,
    });
    await dialog.getByRole('combobox', { name: 'Post-processing model' }).selectOption('gemma3:4b');
    await expect.poll(() => savedPayload).toEqual({
      ...initialSettings,
      silenceSplitMs: 4000,
      cleanup: true,
      cleanupModel: 'gemma3:4b',
    });
    await dialog.getByRole('combobox', { name: 'Title model' }).selectOption('qwen3.8:27b-mlx');
    await expect.poll(() => savedPayload).toEqual({
      ...initialSettings,
      silenceSplitMs: 4000,
      cleanup: true,
      cleanupModel: 'gemma3:4b',
      summaryModel: 'qwen3.8:27b-mlx',
    });
    await dialog.getByRole('switch', { name: 'Generate library titles with local AI (Ollama)' }).click();
    await expect.poll(() => savedPayload).toEqual({
      ...initialSettings,
      silenceSplitMs: 4000,
      cleanup: true,
      cleanupModel: 'gemma3:4b',
      summaryModel: 'qwen3.8:27b-mlx',
      summaries: false,
    });
  } finally {
    await server.close();
  }
});

// ---- Web 3 AC 8, AC 9, AC 10, AC 11, AC 14, AC 16: Connect UI lifecycle, validation, persistence, modal coordination, sanitization ----

test('Connect UI handles validation, cancel, repeated refusal, reload persistence, untouched manual open, shortcuts suspension and zero secret leaks', async ({
  page,
}) => {
  const sentinelToken = 'SENTINEL_WEB3_SECRET_TOKEN_XYZ';
  const wrongToken = 'WRONG_SENTINEL_TOKEN_ABC';
  const server = await startStandInServer({
    distDir: testDistDir,
    token: sentinelToken,
    version: '0.12.0-connect',
  });
  const pageErrors: string[] = [];
  const consoleLines: string[] = [];
  page.on('pageerror', (e) => pageErrors.push(e.message));
  page.on('console', (m) => consoleLines.push(m.text()));

  try {
    await attachWebTestHook(page, { helloMs: 2000, reconnectMs: 100, rpcMs: 500 });
    await openWeb(page, server.url);

    const connectDialog = page.getByRole('dialog', { name: 'Connect' });
    const urlField = connectDialog.getByLabel('Server URL');
    const tokenField = connectDialog.getByLabel('Shared token');
    const submitBtn = connectDialog.getByRole('button', { name: 'Connect', exact: true });
    const cancelBtn = connectDialog.getByRole('button', { name: 'Cancel', exact: true });
    const retryBtn = connectDialog.getByRole('button', { name: 'Try again', exact: true });

    // 1. Initial 4401 opens Connect automatically with page origin pre-filled and blank masked token.
    await expect(connectDialog).toBeVisible();
    await expect(urlField).toHaveValue(server.url);
    await expect(tokenField).toHaveValue('');
    await expect(tokenField).toHaveAttribute('type', 'password');
    await expect(connectDialog.getByRole('alert')).toHaveText('Authentication refused.');
    await expect(retryBtn).toBeVisible();

    const obsInit = await readObservations(page);
    expect(
      obsInit.some(
        (o) =>
          o.category === 'connect' &&
          o.trigger === 'initial_failure' &&
          o.outcome === 'ok' &&
          o.reason === 'open' &&
          o.priorAuth === false &&
          o.failureCode === 'auth_4401' &&
          o.generation > 0,
      ),
    ).toBe(true);

    // 2. Invalid input (ftp://example.invalid & malformed whitespace URL) reports fixed message, mutates nothing, starts no attempt.
    const connsBeforeInvalid = server.totalConnections();
    await urlField.fill('ftp://example.invalid');
    await tokenField.fill('unsaved-draft-token');
    await submitBtn.click();
    await expect(connectDialog.getByRole('alert')).toHaveText('Enter a valid server URL.');
    expect(server.totalConnections()).toBe(connsBeforeInvalid);
    expect(await page.evaluate(() => localStorage.getItem('scriber.web.endpoint'))).toBeNull();
    expect(await page.evaluate(() => localStorage.getItem('scriber.web.token'))).toBeNull();

    await urlField.fill('bad host url');
    await submitBtn.click();
    await expect(connectDialog.getByRole('alert')).toHaveText('Enter a valid server URL.');
    expect(server.totalConnections()).toBe(connsBeforeInvalid);

    // 3. Cancel dismisses Connect, clears token draft without writing/retrying, focuses persistent recovery Connect button.
    await cancelBtn.click();
    await expect(connectDialog).toBeHidden();
    expect(server.totalConnections()).toBe(connsBeforeInvalid);
    expect(await page.evaluate(() => localStorage.getItem('scriber.web.endpoint'))).toBeNull();
    expect(await page.evaluate(() => localStorage.getItem('scriber.web.token'))).toBeNull();
    const recoveryConnect = page.locator('main').getByRole('button', { name: 'Connect', exact: true });
    await expect(recoveryConnect).toBeVisible();
    await expect(recoveryConnect).toBeFocused();
    expect(
      (await readObservations(page)).some(
        (o) => o.category === 'connect' && o.trigger === 'cancel' && o.outcome === 'cancelled' && o.reason === 'dismiss',
      ),
    ).toBe(true);

    // Re-open from persistent recovery trigger -> token draft was cleared! Also exercise Close (×) and backdrop dismissal.
    await recoveryConnect.click();
    await expect(connectDialog).toBeVisible();
    await expect(urlField).toHaveValue(server.url);
    await expect(tokenField).toHaveValue('');
    await connectDialog.getByRole('button', { name: 'Close', exact: true }).click();
    await expect(connectDialog).toBeHidden();
    await recoveryConnect.click();
    await expect(connectDialog).toBeVisible();
    await page.mouse.click(5, 5);
    await expect(connectDialog).toBeHidden();
    await recoveryConnect.click();
    await expect(connectDialog).toBeVisible();
    await expect(urlField).toHaveValue(server.url);
    await expect(tokenField).toHaveValue('');

    // 4. Valid refused save replaces BOTH storage keys immediately, opens no auto-retry, leaves form editable.
    await urlField.fill(''); // blank origin means page origin
    await tokenField.fill(wrongToken);
    await submitBtn.click();
    await expect(connectDialog.getByRole('alert')).toHaveText('Authentication refused.');
    expect(await page.evaluate(() => localStorage.getItem('scriber.web.endpoint'))).toBe(server.url);
    expect(await page.evaluate(() => localStorage.getItem('scriber.web.token'))).toBe(wrongToken);
    const connsAfterRefusedSave = server.totalConnections();
    await page.waitForTimeout(250);
    expect(server.totalConnections()).toBe(connsAfterRefusedSave); // no auto-retry!
    await expect(urlField).toBeEditable();
    await expect(tokenField).toBeEditable();

    // 5. Valid credentials authenticate, dismiss Connect via active_auth, restore recorder focus, bootstrap & refresh.
    await urlField.fill(''); // blank origin -> page origin
    await tokenField.fill(sentinelToken);
    await submitBtn.click();
    await expect(connectDialog).toBeHidden();
    await expect(status(page)).toHaveText('Click New to start a dictation.');
    expect(await page.evaluate(() => localStorage.getItem('scriber.web.endpoint'))).toBe(server.url);
    expect(await page.evaluate(() => localStorage.getItem('scriber.web.token'))).toBe(sentinelToken);
    expect(
      (await readObservations(page)).some(
        (o) => o.category === 'connect' && o.trigger === 'active_auth' && o.outcome === 'ok' && o.reason === 'dismiss',
      ),
    ).toBe(true);
    // Usable focus restored in the recorder (New button is primary & enabled in idle).
    await expect(button(page, 'New')).toBeFocused();

    // 6. Persistence across reload: page reloads and authenticates without opening Connect.
    await page.reload();
    await expect(page.locator('html')).toHaveAttribute('data-bridge', 'web');
    await expect(status(page)).toHaveText('Click New to start a dictation.');
    await expect(connectDialog).toBeHidden();

    // 7. Manual open from Settings: explicitly closes Settings first (only one :modal), suspends recorder shortcuts,
    //    background auth leaves untouched manual Connect open, Escape reopens Settings with focus on Connect.
    await press(page, 'New');
    await expect(status(page)).toHaveText('Click Record and start talking.');
    await button(page, 'Settings').click();
    const settingsDialog = page.getByRole('dialog', { name: 'Settings' });
    await expect(settingsDialog).toBeVisible();
    await settingsDialog.getByRole('button', { name: 'Connect', exact: true }).click();
    await expect(settingsDialog).toBeHidden();
    await expect(connectDialog).toBeVisible();
    expect(await page.evaluate(() => document.querySelectorAll('dialog:modal').length)).toBe(1);
    expect(
      (await readObservations(page)).some(
        (o) => o.category === 'connect' && o.trigger === 'settings' && o.outcome === 'ok' && o.reason === 'open',
      ),
    ).toBe(true);

    // Recorder shortcuts are suspended while Connect is open (Space does not start recording, Meta+Comma does not open Settings).
    const actsBeforeKeys = server.log.filter((e) => e.kind === 'rpc' && e.call === 'dictation_act').length;
    await page.keyboard.press('Meta+n');
    await page.keyboard.press('Meta+Comma');
    await expect(settingsDialog).toBeHidden();
    expect(server.log.filter((e) => e.kind === 'rpc' && e.call === 'dictation_act').length).toBe(actsBeforeKeys);

    // Background reconnect auth leaves untouched manual Connect open!
    server.disconnectAll(1001, 'bg drop');
    await expect
      .poll(async () =>
        (await readObservations(page)).some(
          (o) => o.category === 'connect' && o.trigger === 'background_auth' && o.outcome === 'skip' && o.reason === 'keep_open',
        ),
      )
      .toBe(true);
    await expect(connectDialog).toBeVisible();

    // Typing a draft token then pressing Escape clears draft, preserves storage, reopens Settings with focus on Connect.
    await tokenField.fill('temporary-draft-never-saved');
    await page.keyboard.press('Escape');
    await expect(connectDialog).toBeHidden();
    await expect(settingsDialog).toBeVisible();
    await expect(settingsDialog.getByRole('button', { name: 'Connect', exact: true })).toBeFocused();
    expect(await page.evaluate(() => localStorage.getItem('scriber.web.token'))).toBe(sentinelToken);

    // Verify blank token submits empty string (when server allows empty token).
    server.setToken('');
    await settingsDialog.getByRole('button', { name: 'Connect', exact: true }).click();
    await expect(connectDialog).toBeVisible();
    await expect(tokenField).toHaveValue('');
    // Explicit same-origin configure failure after prior auth reports Could not connect and keeps Connect open.
    // Keep the background reconnect delay wide so the explicit retry click below cannot race a 100 ms auto-reconnect.
    await page.evaluate(() => {
      if (window.__scriberWebTest !== undefined) {
        window.__scriberWebTest.timeouts = { helloMs: 2000, reconnectMs: 5000, rpcMs: 500 };
      }
    });
    server.setHelloMode('close_before_hello');
    await submitBtn.click();
    await expect(connectDialog.getByRole('alert')).toHaveText('Could not connect to the Scriber server.');
    expect(
      (await readObservations(page)).some(
        (o) => o.category === 'connect' && o.trigger === 'explicit_failure' && o.priorAuth === true,
      ),
    ).toBe(true);
    server.setHelloMode('ok');
    await submitBtn.click();
    await expect(connectDialog).toBeHidden();
    await expect(settingsDialog).toBeVisible();
    await expect(settingsDialog.getByRole('button', { name: 'Connect', exact: true })).toBeFocused();
    expect(await page.evaluate(() => localStorage.getItem('scriber.web.token'))).toBe('');

    // Verify normalizeOrigin HTTPS loopback / remote rules in mapped coverage context.
    const httpsNorm = await page.evaluate(() => {
      const b = window.__scriberWebTest?.bridge as { normalizeOrigin: (raw: string, base?: string) => string };
      const allowed = [
        b.normalizeOrigin('https://remote.example:8742/p?q=1#h', 'https://scriber.test'),
        b.normalizeOrigin('wss://remote.example:8742/ws', 'https://scriber.test'),
        b.normalizeOrigin('http://localhost:8742', 'https://scriber.test'),
        b.normalizeOrigin('http://sub.localhost:8742', 'https://scriber.test'),
        b.normalizeOrigin('http://127.0.0.1:8742', 'https://scriber.test'),
        b.normalizeOrigin('http://[::1]:8742', 'https://scriber.test'),
      ];
      let rejected = '';
      try {
        b.normalizeOrigin('http://remote.example:8742', 'https://scriber.test');
      } catch (e) {
        rejected = e instanceof Error ? e.message : String(e);
      }
      return { allowed, rejected };
    });
    expect(httpsNorm.allowed).toEqual([
      'https://remote.example:8742',
      'https://remote.example:8742',
      'http://localhost:8742',
      'http://sub.localhost:8742',
      'http://127.0.0.1:8742',
      'http://[::1]:8742',
    ]);
    expect(httpsNorm.rejected).toContain('https or wss');

    // Sanitization check: sentinel token matched ONLY via boolean authMatch in server hello log and appears NOWHERE else!
    expect(server.log.some((e) => e.kind === 'hello' && e.authMatch)).toBe(true);
    const bodyText = (await page.locator('body').textContent()) ?? '';
    const clientObs = JSON.stringify(await readObservations(page));
    const serverLog = JSON.stringify(server.log);
    for (const secret of [sentinelToken, wrongToken, 'temporary-draft-never-saved']) {
      expect(page.url()).not.toContain(secret);
      expect(bodyText).not.toContain(secret);
      expect(pageErrors.join('\n')).not.toContain(secret);
      expect(consoleLines.join('\n')).not.toContain(secret);
      expect(clientObs).not.toContain(secret);
      expect(serverLog).not.toContain(secret);
    }
  } finally {
    await server.close();
  }
});

// ---- Web 3 AC 9: HTTPS page rejects insecure non-loopback remote origins before mutation ----

test('HTTPS page rejects insecure non-loopback remote origins in Connect before mutating storage or opening sockets', async ({
  page,
}) => {
  await page.route('https://scriber.test/**', async (route) => {
    const reqUrl = new URL(route.request().url());
    const rel = reqUrl.pathname === '/' ? 'index.html' : reqUrl.pathname.replace(/^\/+/, '');
    const filePath = join(testDistDir, rel);
    if (!existsSync(filePath)) {
      await route.fulfill({ status: 404, body: 'Not found' });
      return;
    }
    const ext = filePath.slice(filePath.lastIndexOf('.'));
    const types: Record<string, string> = {
      '.html': 'text/html; charset=utf-8',
      '.js': 'text/javascript; charset=utf-8',
      '.css': 'text/css; charset=utf-8',
      '.webmanifest': 'application/manifest+json; charset=utf-8',
      '.png': 'image/png',
    };
    await route.fulfill({
      status: 200,
      contentType: types[ext] ?? 'application/octet-stream',
      body: readFileSync(filePath),
    });
  });

  await page.addInitScript(() => {
    const w = window as unknown as { __wsUrls: string[] };
    w.__wsUrls = [];
    class StubWs {
      static CONNECTING = 0;
      static OPEN = 1;
      static CLOSING = 2;
      static CLOSED = 3;
      readyState = 0;
      binaryType = 'blob';
      onopen: (() => void) | null = null;
      onmessage: ((e: MessageEvent) => void) | null = null;
      onclose: ((e: CloseEvent) => void) | null = null;
      onerror: ((e: Event) => void) | null = null;
      constructor(url: string) {
        w.__wsUrls.push(url);
        setTimeout(() => {
          this.readyState = 3;
          this.onclose?.(new CloseEvent('close', { code: 4401 }));
        }, 10);
      }
      send(): void {
        // no-op
      }
      close(): void {
        this.readyState = 3;
      }
    }
    Object.defineProperty(window, 'WebSocket', { configurable: true, value: StubWs });
  });

  await page.goto('https://scriber.test/');
  const connectDialog = page.getByRole('dialog', { name: 'Connect' });
  await expect(connectDialog).toBeVisible();
  const wsBefore = await page.evaluate(() => (window as unknown as { __wsUrls: string[] }).__wsUrls.length);

  for (const bad of ['http://remote.example:8742', 'ws://remote.example:8742', 'ftp://example.invalid']) {
    await connectDialog.getByLabel('Server URL').fill(bad);
    await connectDialog.getByRole('button', { name: 'Connect', exact: true }).click();
    await expect(connectDialog.getByRole('alert')).toHaveText('Enter a valid server URL.');
  }
  expect(await page.evaluate(() => (window as unknown as { __wsUrls: string[] }).__wsUrls.length)).toBe(wsBefore);
  expect(await page.evaluate(() => localStorage.getItem('scriber.web.endpoint'))).toBeNull();

  // Loopback HTTP from HTTPS is permitted.
  await connectDialog.getByLabel('Server URL').fill('http://127.0.0.1:8742');
  await connectDialog.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect
    .poll(() => page.evaluate(() => (window as unknown as { __wsUrls: string[] }).__wsUrls.at(-1)))
    .toBe('ws://127.0.0.1:8742/ws');
});

// ---- Web 3 AC 6, AC 7, AC 11, AC 15, AC 16: Startup failures, sync socket error, post-auth 4401, transient suppression, retired A vs B races ----

test('handles all initial startup handshake failures on same-origin reloads', async ({ page }) => {
  const srv = await startStandInServer({ distDir: testDistDir, helloMode: 'hang' });
  try {
    await attachWebTestHook(page, { helloMs: 120, reconnectMs: 5_000, rpcMs: 500 });
    for (const mode of ['hang', 'invalid_json', 'bad_platform', 'close_before_hello', 'close_1009_before_hello'] as const) {
      srv.setHelloMode(mode);
      await openWeb(page, srv.url);
      const dlg = page.getByRole('dialog', { name: 'Connect' });
      await expect(dlg).toBeVisible();
      await expect(dlg.getByRole('alert')).toHaveText('Could not connect to the Scriber server.');
      const expectedCode =
        mode === 'hang'
          ? 'hello_timeout'
          : mode === 'invalid_json' || mode === 'bad_platform'
            ? 'invalid_hello'
            : mode === 'close_1009_before_hello'
              ? 'message_too_big'
              : 'closed_before_hello';
      expect(
        (await readObservations(page)).some(
          (o) =>
            o.category === 'connect' &&
            o.trigger === 'initial_failure' &&
            o.outcome === 'ok' &&
            o.reason === 'open' &&
            o.priorAuth === false &&
            o.failureCode === expectedCode,
        ),
      ).toBe(true);
    }
  } finally {
    await srv.close();
  }
});

test('handles synchronous WebSocket construction failure and recovers on explicit Try again', async ({ page }) => {
  const syncServer = await startStandInServer({ distDir: testDistDir });
  try {
    await attachWebTestHook(page, { helloMs: 250, reconnectMs: 100 });
    await page.addInitScript(() => {
      const RealWs = window.WebSocket;
      let throwOnce = true;
      const Wrapped = function (this: unknown, url: string | URL, protocols?: string | string[]) {
        if (throwOnce) {
          throwOnce = false;
          throw new DOMException('blocked by test', 'SecurityError');
        }
        return protocols === undefined ? new RealWs(url) : new RealWs(url, protocols);
      } as unknown as typeof WebSocket;
      Object.assign(Wrapped, RealWs);
      Wrapped.prototype = RealWs.prototype;
      Object.defineProperty(window, 'WebSocket', { configurable: true, value: Wrapped });
    });
    await openWeb(page, syncServer.url);
    const dlg = page.getByRole('dialog', { name: 'Connect' });
    await expect(dlg).toBeVisible();
    await expect(dlg.getByRole('alert')).toHaveText('Could not connect to the Scriber server.');
    expect(
      (await readObservations(page)).some(
        (o) => o.category === 'connect' && o.trigger === 'initial_failure' && o.failureCode === 'socket_error',
      ),
    ).toBe(true);
    // Cancel to recovery view and click main Try again -> recovers cleanly without re-entering credentials.
    await page.keyboard.press('Escape');
    await expect(dlg).toBeHidden();
    await page.locator('main').getByRole('button', { name: 'Try again', exact: true }).click();
    await expect(status(page)).toHaveText('Click New to start a dictation.');
  } finally {
    await syncServer.close();
  }
});

test('handles post-auth 4401 interrupting Settings, transient reconnect suppression, and retired socket A vs B races', async ({
  page,
}) => {
  const serverA = await startStandInServer({ distDir: testDistDir, version: '1.0.0-server-A' });
  const serverB = await startStandInServer({ distDir: testDistDir, version: '2.0.0-server-B', helloMode: 'hold' });
  try {
    await attachWebTestHook(page, { helloMs: 2000, reconnectMs: 80, rpcMs: 500 });
    // Spy on WebSocket instances and retain original handlers even after retire() nulls them.
    await page.addInitScript(() => {
      interface SpyRecord {
        url: string;
        onopen: (() => void) | null;
        onmessage: ((ev: MessageEvent) => void) | null;
        onclose: ((ev: CloseEvent) => void) | null;
      }
      const w = window as unknown as { __wsSpy: SpyRecord[] };
      w.__wsSpy = [];
      const RealWs = window.WebSocket;
      class SpyWebSocket extends RealWs {
        private readonly rec: SpyRecord;
        constructor(url: string | URL, protocols?: string | string[]) {
          super(url, protocols);
          this.rec = { url: String(url), onopen: null, onmessage: null, onclose: null };
          w.__wsSpy.push(this.rec);
        }
        override set onopen(fn: ((this: WebSocket, ev: Event) => unknown) | null) {
          if (fn !== null) this.rec.onopen = fn as () => void;
          super.onopen = fn;
        }
        override get onopen() {
          return super.onopen;
        }
        override set onmessage(fn: ((this: WebSocket, ev: MessageEvent) => unknown) | null) {
          if (fn !== null) this.rec.onmessage = fn as (ev: MessageEvent) => void;
          super.onmessage = fn;
        }
        override get onmessage() {
          return super.onmessage;
        }
        override set onclose(fn: ((this: WebSocket, ev: CloseEvent) => unknown) | null) {
          if (fn !== null) this.rec.onclose = fn;
          super.onclose = fn;
        }
        override get onclose() {
          return super.onclose;
        }
      }
      Object.defineProperty(window, 'WebSocket', { configurable: true, value: SpyWebSocket });
    });

    // First successful hello -> Connect stays closed.
    await openWeb(page, serverA.url);
    await expect(status(page)).toHaveText('Click New to start a dictation.');
    const connectDialog = page.getByRole('dialog', { name: 'Connect' });
    await expect(connectDialog).toBeHidden();

    // Transient pre-hello reconnect failure after prior auth -> Connect stays CLOSED and records suppression!
    serverA.setHelloMode('close_before_hello');
    serverA.disconnectAll(1001, 'transient drop');
    await expect
      .poll(async () =>
        (await readObservations(page)).some(
          (o) =>
            o.category === 'connect' &&
            o.trigger === 'transient_reconnect' &&
            o.outcome === 'skip' &&
            o.reason === 'suppress' &&
            o.priorAuth === true &&
            o.failureCode === 'closed_before_hello',
        ),
      )
      .toBe(true);
    await expect(connectDialog).toBeHidden();
    serverA.setHelloMode('ok');
    await expect(status(page)).toHaveText('Click New to start a dictation.');

    // Post-auth 4401 interrupts open Settings; dismissing returns focus to persistent Connect recovery trigger in band!
    await button(page, 'Settings').click();
    const settingsDialog = page.getByRole('dialog', { name: 'Settings' });
    await expect(settingsDialog).toBeVisible();
    serverA.setToken('rotated-token');
    serverA.disconnectAll(4401, 'token rotated');
    await expect(settingsDialog).toBeHidden();
    await expect(connectDialog).toBeVisible();
    await expect(connectDialog.getByRole('alert')).toHaveText('Authentication refused.');
    expect(
      (await readObservations(page)).some(
        (o) =>
          o.category === 'connect' &&
          o.trigger === 'post_auth_4401' &&
          o.outcome === 'ok' &&
          o.reason === 'open' &&
          o.priorAuth === true &&
          o.failureCode === 'auth_4401',
      ),
    ).toBe(true);

    // Dismissing auto recovery returns focus to persistent Connect button (NOT Settings).
    await page.keyboard.press('Escape');
    await expect(connectDialog).toBeHidden();
    await expect(settingsDialog).toBeHidden();
    const bandConnect = page.locator('.band').getByRole('button', { name: 'Connect', exact: true });
    await expect(bandConnect).toBeVisible();
    await expect(bandConnect).toBeFocused();

    // Race 1 & Race 2: Switch to Server B (whose hello is held).
    // Capture retired A socket index before switching.
    const spyCountBeforeB = await page.evaluate(
      () => (window as unknown as { __wsSpy: unknown[] }).__wsSpy.length,
    );
    await bandConnect.click();
    await expect(connectDialog).toBeVisible();
    await connectDialog.getByLabel('Server URL').fill(serverB.url);
    await connectDialog.getByLabel('Shared token').fill('');
    await connectDialog.getByRole('button', { name: 'Connect', exact: true }).click();
    await expect.poll(() => serverB.pendingHeldHellos()).toBe(1);

    // Deterministically deliver retired A's late onopen, valid hello AND stale snapshot WHILE B is pending!
    await page.evaluate((idx) => {
      const spy = (
        window as unknown as {
          __wsSpy: Array<{
            onopen: (() => void) | null;
            onmessage: ((ev: MessageEvent) => void) | null;
          }>;
        }
      ).__wsSpy;
      const retiredA = spy[idx - 1];
      retiredA?.onopen?.();
      retiredA?.onmessage?.(
        new MessageEvent('message', {
          data: JSON.stringify({ hello: { version: '9.9.9-stale-A', platform: 'web' } }),
        }),
      );
      retiredA?.onmessage?.(
        new MessageEvent('message', {
          data: JSON.stringify({
            event: 'dictation-changed',
            payload: { revision: 99, phase: 'ready', text: '', recordedMs: 0, segments: 0, pending: 0, error: null, notice: null, modelError: null, download: null, libraryRevision: 0, microphone: null, learned: [], partial: '', rawText: null, postProcessing: false },
          }),
        }),
      );
    }, spyCountBeforeB);

    // B must still be pending and Connect must still be open!
    await expect(connectDialog).toBeVisible();

    // Now release B's held hello -> B authenticates, bootstraps, and closes Connect.
    serverB.releaseHeldHellos();
    await expect(connectDialog).toBeHidden();
    await expect(status(page)).toHaveText('Click New to start a dictation.');

    // Now deterministically deliver retired A's late 4401 AFTER B has authenticated!
    const obsCountBeforeLate4401 = (await readObservations(page)).length;
    await page.evaluate((idx) => {
      const spy = (window as unknown as { __wsSpy: Array<{ onclose: ((ev: CloseEvent) => void) | null }> }).__wsSpy;
      const retiredA = spy[idx - 1];
      retiredA?.onclose?.(new CloseEvent('close', { code: 4401, reason: 'late 4401 from A' }));
    }, spyCountBeforeB);
    await page.waitForTimeout(80);

    // B's modal stays closed, auth stays active, and RPCs + Settings version prove we are cleanly on B!
    await expect(connectDialog).toBeHidden();
    const obsAfterLate4401 = (await readObservations(page)).slice(obsCountBeforeLate4401);
    expect(obsAfterLate4401.some((o) => o.reason === 'auth_4401' || o.trigger === 'post_auth_4401')).toBe(false);
    await button(page, 'Settings').click();
    await expect(page.getByRole('dialog', { name: 'Settings' }).getByText('Version 2.0.0-server-B')).toBeVisible();
    await page.keyboard.press('Escape');
    await press(page, 'New');
    await expect(status(page)).toHaveText('Click Record and start talking.');
  } finally {
    await serverA.close();
    await serverB.close();
  }
});

// ---- Web 3 AC 12: Changed normalized origin clears state, disables writes until new Settings load, blocks stale completions ----

test('changing normalized origin clears prior server data, blocks stale completions, and disables settings writes until new server settings load', async ({
  page,
}) => {
  const sharedEntryId = '20261122-100000';
  const settingsA: Settings = {
    summaries: true,
    libraryOpen: true,
    vocabulary: '/server-a/vocab.txt',
    cleanup: true,
    summaryModel: 'gemma3:4b',
    cleanupModel: 'qwen3.8:27b-mlx',
    libraryWidth: 220,
    inputDevice: 'Mic A',
    silenceSplitMs: 1500,
    keepSegments: true,
    learnedVocabulary: '/server-a/learned.txt',
    modelFolder: '/server-a/models',
    toggleShortcut: 'Opt+Cmd+A',
    pauseShortcut: 'Opt+Cmd+B',
    copySound: true,
  };
  const settingsB: Settings = {
    summaries: false,
    libraryOpen: true,
    vocabulary: '/server-b/vocab.txt',
    cleanup: false,
    summaryModel: 'qwen3.8:27b-mlx',
    cleanupModel: 'gemma3:4b',
    libraryWidth: 240,
    inputDevice: 'Mic B',
    silenceSplitMs: 4000,
    keepSegments: false,
    learnedVocabulary: '/server-b/learned.txt',
    modelFolder: '/server-b/models',
    toggleShortcut: 'Opt+Cmd+X',
    pauseShortcut: 'Opt+Cmd+Y',
    copySound: false,
  };

  let releaseStaleEntryA: (() => void) | null = null;
  let releaseSettingsB: (() => void) | null = null;
  const savedOnB: Settings[] = [];

  const serverA = await startStandInServer({
    distDir: testDistDir,
    version: '1.0.0-A',
    initialSettings: settingsA,
    initialLibrary: [
      {
        entry: { id: sharedEntryId, title: '2026-11-22 10:00:00', summary: 'Summary from Server A' },
        transcript: 'Transcript from Server A.',
      },
    ],
  });
  const serverB = await startStandInServer({
    distDir: testDistDir,
    version: '2.0.0-B',
    initialSettings: settingsB,
    initialLibrary: [
      {
        entry: { id: sharedEntryId, title: '2026-11-22 10:00:00', summary: 'Summary from Server B' },
        transcript: 'Transcript from Server B.',
      },
    ],
    onCall: (ctx) => {
      if (ctx.call === 'settings_get' && releaseSettingsB === null) {
        releaseSettingsB = () => {
          ctx.replyOk(settingsB);
        };
        return true;
      }
      if (ctx.call === 'settings_set') {
        savedOnB.push(structuredClone(ctx.args.settings as Settings));
      }
      return false;
    },
  });

  try {
    await attachWebTestHook(page, { reconnectMs: 100 });
    await openWeb(page, serverA.url);

    // Trigger former-owner recording notice on Server A, then select overlapping entry with a held library_get.
    await press(page, 'New', 'Record');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    serverA.disconnectAll(1001, 'drop A');
    await expect(status(page)).toHaveText('Connection lost. Press Done to finish or Discard to start over.');

    serverA.setOnCall((ctx) => {
      if (ctx.call === 'library_get' && ctx.args.id === sharedEntryId) {
        releaseStaleEntryA = () => {
          ctx.replyOk('Stale delayed transcript from Server A.');
        };
        return true;
      }
      return false;
    });
    await page.locator('#library li').first().getByRole('button').click();
    await expect(page.getByRole('heading', { level: 2 })).toHaveText('2026-11-22 10:00:00');
    await expect.poll(() => releaseStaleEntryA !== null).toBe(true);

    // Switch from Server A to Server B via Settings -> Connect.
    await button(page, 'Settings').click();
    const settingsDialog = page.getByRole('dialog', { name: 'Settings' });
    await expect(settingsDialog).toBeVisible();
    await settingsDialog.getByRole('button', { name: 'Connect', exact: true }).click();
    const connectDialog = page.getByRole('dialog', { name: 'Connect' });
    await expect(connectDialog).toBeVisible();
    await connectDialog.getByLabel('Server URL').fill(serverB.url);
    await connectDialog.getByRole('button', { name: 'Connect', exact: true }).click();

    // Server B authenticates -> Connect closes, Settings reopens, settings writes MUST be disabled while settings_get is held!
    await expect(connectDialog).toBeHidden();
    await expect(settingsDialog).toBeVisible();
    await expect.poll(() => releaseSettingsB !== null).toBe(true);
    await expect(settingsDialog.getByLabel('Split into parts at a pause')).toBeDisabled();
    await expect(settingsDialog.getByRole('switch', { name: 'Enable post-processing with local AI (Ollama)' })).toBeDisabled();
    await expect(settingsDialog.getByRole('switch', { name: 'Generate library titles with local AI (Ollama)' })).toBeDisabled();

    // Release Server A's stale library_get completion: must be ignored!
    const releaseA = releaseStaleEntryA as (() => void) | null;
    releaseA?.();
    await page.waitForTimeout(50);

    // Even closing Settings and clicking Library toggle while settings_get is still pending sends NO settings_set!
    await page.keyboard.press('Escape');
    await button(page, 'Library').click();
    await page.waitForTimeout(30);
    expect(savedOnB).toHaveLength(0);
    await button(page, 'Settings').click();
    await expect(settingsDialog).toBeVisible();

    // Release Server B's settings_get -> controls unlock with Server B's values!
    const releaseB = releaseSettingsB as (() => void) | null;
    releaseB?.();
    await expect(settingsDialog.getByLabel('Split into parts at a pause')).toBeEnabled();
    await expect(settingsDialog.getByLabel('Split into parts at a pause')).toHaveValue('4000');
    await expect(settingsDialog.getByRole('switch', { name: 'Enable post-processing with local AI (Ollama)' })).not.toBeChecked();
    await expect(settingsDialog.getByRole('switch', { name: 'Generate library titles with local AI (Ollama)' })).not.toBeChecked();
    await expect(settingsDialog.getByText('Version 2.0.0-B')).toBeVisible();

    // First save on Server B carries Server B's COMPLETE Settings including every hidden value!
    await settingsDialog.getByRole('switch', { name: 'Enable post-processing with local AI (Ollama)' }).click();
    await expect.poll(() => savedOnB.length).toBe(1);
    expect(savedOnB[0]).toEqual({ ...settingsB, cleanup: true });

    // Close Settings -> verify prior Server A entry selection & former-owner notice are gone, and Server B's entry loads cleanly!
    await page.keyboard.press('Escape');
    await expect(page.getByRole('heading', { level: 2 })).toHaveCount(0);
    await expect(status(page)).toHaveText('Click New to start a dictation.');
    await expect(page.getByText('Summary from Server B')).toBeVisible();
    await expect(page.getByText('Summary from Server A')).toHaveCount(0);
    await page.locator('#library li').first().getByRole('button').click();
    await expect(page.locator('#entry-transcript')).toHaveValue('Transcript from Server B.');
  } finally {
    await serverA.close();
    await serverB.close();
  }
});
