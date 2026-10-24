import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { expect, test } from '@playwright/test';

import { byDisplayOrder, extensionOf, Mock, MOCK_ROOT, MOCK_VERSION, tree, walk } from '../../src/lib/mock';
import type { ExtensionSummary, ScanProgress } from '../../src/lib/types';

// The mock must answer the way scriber-core does (docs/design.md), or the UI
// is tested against behaviour the real app never produces. Three checks:
// the mock's JSON shapes match the Rust fixture that the core's serde tests
// pin; its rules match a transcription of the core's; and it identifies
// itself, so it can never pass for the real app.

const FIXTURE = resolve(process.cwd(), '../../crates/scriber-core/tests/fixtures/bridge-shapes.json');
const shapes = JSON.parse(readFileSync(FIXTURE, 'utf8')) as Record<string, unknown>;

/** The value's shape: each key and the type of its value, recursively (arrays by their first element). */
function shape(v: unknown): unknown {
  if (Array.isArray(v)) return [shape(v[0])];
  if (v !== null && typeof v === 'object') {
    return Object.fromEntries(
      Object.entries(v)
        .sort(([a], [b]) => (a < b ? -1 : 1))
        .map(([k, x]) => [k, shape(x)]),
    );
  }
  return typeof v;
}

test.describe('the mock speaks the Rust JSON shapes', () => {
  test('app_info', async () => {
    expect(shape(await new Mock('').appInfo())).toEqual(shape(shapes.appInfo));
  });

  test('scan_folder and its progress events', async () => {
    const mock = new Mock('');
    const events: ScanProgress[] = [];
    mock.onProgress((p) => events.push(p));
    const summary = await mock.scanFolder(MOCK_ROOT);
    expect(shape(summary)).toEqual(shape(shapes.scanSummary));
    expect(events.length).toBeGreaterThan(0);
    for (const e of events) expect(shape(e)).toEqual(shape(shapes.scanProgress));
  });
});

// ---- the core's rules, transcribed -----------------------------------------
//
// From crates/scriber-core/src/scan.rs. If the core's rule changes, this
// changes with it and the mock must follow.

function coreExtension(name: string): string {
  // `rsplit_once('.')`, and no extension when the stem before the last dot is empty.
  const i = name.lastIndexOf('.');
  if (i === -1) return '';
  const stem = name.slice(0, i);
  return stem === '' ? '' : name.slice(i + 1).toLowerCase();
}

test('the extension rule is the core rule', () => {
  // The same cases as the core's own unit test, and a few more.
  for (const name of ['photo.JPG', 'archive.tar.gz', 'Makefile', '.profile', '..', 'trailing.', 'a.b', 'Ünïcode.ÄBC', '', '.']) {
    expect(extensionOf(name), name).toBe(coreExtension(name));
  }
  expect(extensionOf('Ünïcode.ÄBC')).toBe('äbc');
});

test('the display order is the core order: bytes, then files, then name', () => {
  const rows: ExtensionSummary[] = [
    { extension: 'b', files: 1, bytes: 10 },
    { extension: 'a', files: 1, bytes: 10 },
    { extension: 'c', files: 2, bytes: 10 },
    { extension: 'd', files: 9, bytes: 5 },
    { extension: 'e', files: 1, bytes: 50 },
  ];
  expect([...rows].sort(byDisplayOrder).map((r) => r.extension)).toEqual(['e', 'c', 'a', 'b', 'd']);
  const same = { extension: 'a', files: 1, bytes: 1 };
  expect(byDisplayOrder(same, { ...same })).toBe(0);
});

test('the walk counts like the core: links skipped, unreadable subfolders counted', () => {
  const reports: ScanProgress[] = [];
  const summary = walk(tree('photos'), MOCK_ROOT, (p) => reports.push(p));
  expect(summary).toEqual({
    root: MOCK_ROOT,
    files: 6,
    totalBytes: 57_607_348,
    unreadableFolders: 1,
    extensions: [
      { extension: 'dng', files: 2, bytes: 49_500_000 },
      { extension: 'jpg', files: 2, bytes: 8_100_000 },
      { extension: '', files: 1, bytes: 6_148 },
      { extension: 'txt', files: 1, bytes: 1_200 },
    ],
    elapsedMs: 0,
  });
  // The last report is the final totals, as in the core.
  expect(reports.at(-1)).toEqual({ files: 6, bytes: 57_607_348, folders: 2 });
});

test('an unreadable or missing root is the core error sentence', () => {
  expect(() => walk(tree('locked'), MOCK_ROOT, () => undefined)).toThrow(
    `${MOCK_ROOT} could not be read: Permission denied (os error 13)`,
  );
  expect(() => walk(tree('photos'), '/nowhere', () => undefined)).toThrow(
    '/nowhere could not be read: No such file or directory (os error 2)',
  );
});

test('the mock identifies itself', async () => {
  expect((await new Mock('').appInfo()).version).toBe(MOCK_VERSION);
  expect(MOCK_VERSION).toMatch(/mock/);
});

test('a command named in ?fail= rejects, and only that one', async () => {
  const mock = new Mock('fail=app_info');
  await expect(mock.appInfo()).rejects.toThrow('Mock failure: app_info');
  await expect(mock.scanFolder(MOCK_ROOT)).resolves.toMatchObject({ root: MOCK_ROOT });
});

test('the picker can be cancelled, and a held scan waits for release', async () => {
  expect(new Mock('pick=cancel').pickFolder()).toBeNull();
  const mock = new Mock('tree=empty');
  mock.handle.hold('scan_folder');
  let done = false;
  const pending = mock.scanFolder(MOCK_ROOT).then(() => {
    done = true;
  });
  await Promise.resolve();
  expect(done).toBe(false);
  mock.handle.release('scan_folder');
  await pending;
  expect(done).toBe(true);
  expect(mock.handle.scans).toEqual([MOCK_ROOT]);
});
