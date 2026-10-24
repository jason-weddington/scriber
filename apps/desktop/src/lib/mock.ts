// A deterministic stand-in for the Rust core, used whenever the frontend runs
// outside Tauri (Playwright, a plain browser). It answers every bridge call
// the way crates/scriber-core does, over a small in-memory folder tree, and
// tests/unit/mock.spec.ts holds it to that: same JSON shapes as the Rust
// fixture, same extension rule, same order, same error text.
//
// The page URL picks the scenario:
//   ?tree=photos (default) | many | empty | locked   what the picked folder holds
//   ?pick=cancel                                      the picker is dismissed
//   ?fail=<command>                                   that command rejects
// A test can hold a command so the UI is seen mid-call:
//   window.__mockBridge.hold('scan_folder') ... window.__mockBridge.release('scan_folder')

import type { AppInfo, ExtensionSummary, ScanProgress, ScanSummary } from './types';

/** The version the mock reports, so a mock can never pass for the real app. */
export const MOCK_VERSION = '0.0.0-mock';

/** The folder the mock's picker returns. */
export const MOCK_ROOT = '/Users/me/Pictures';

export type Entry =
  | { name: string; kind: 'file'; size: number }
  | { name: string; kind: 'folder' }
  | { name: string; kind: 'other' };

/** A folder tree: each folder path maps to its listing, or to the error reading it gives. */
export type Tree = Map<string, Entry[] | { error: string }>;

const file = (name: string, size: number): Entry => ({ name, kind: 'file', size });
const folder = (name: string): Entry => ({ name, kind: 'folder' });

/** The trees the `tree` URL parameter names. */
export function tree(name: string): Tree {
  switch (name) {
    case 'many':
      return new Map([
        [
          MOCK_ROOT,
          Array.from({ length: 40 }, (_, i) => file(`file.x${String(i + 1).padStart(2, '0')}`, (40 - i) * 1_000_000)),
        ],
      ]);
    case 'empty':
      return new Map([[MOCK_ROOT, []]]);
    case 'locked':
      return new Map([[MOCK_ROOT, { error: 'Permission denied (os error 13)' }]]);
    default:
      return new Map<string, Entry[] | { error: string }>([
        [
          MOCK_ROOT,
          [
            file('IMG_0001.JPG', 4_200_000),
            file('IMG_0002.jpg', 3_900_000),
            file('notes.txt', 1_200),
            file('.DS_Store', 6_148),
            folder('raw'),
            folder('locked'),
            { name: 'latest', kind: 'other' },
          ],
        ],
        [`${MOCK_ROOT}/raw`, [file('IMG_0001.DNG', 25_000_000), file('IMG_0002.DNG', 24_500_000)]],
        [`${MOCK_ROOT}/locked`, { error: 'Permission denied (os error 13)' }],
      ]);
  }
}

/** The core's extension rule (crates/scriber-core/src/scan.rs `extension_of`). */
export function extensionOf(name: string): string {
  const dot = name.lastIndexOf('.');
  return dot <= 0 ? '' : name.slice(dot + 1).toLowerCase();
}

/** The core's display order (crates/scriber-core/src/scan.rs `summarise`). */
export function byDisplayOrder(a: ExtensionSummary, b: ExtensionSummary): number {
  return b.bytes - a.bytes || b.files - a.files || (a.extension < b.extension ? -1 : a.extension > b.extension ? 1 : 0);
}

/**
 * The core's walk (crates/scriber-core/src/scan.rs `scan`): every regular file
 * below the root, links not followed, an unreadable subfolder counted and
 * skipped, an unreadable root an error. Reports progress after each folder.
 */
export function walk(t: Tree, root: string, report: (p: ScanProgress) => void): ScanSummary {
  const progress: ScanProgress = { files: 0, bytes: 0, folders: 0 };
  const totals = new Map<string, ExtensionSummary>();
  let unreadableFolders = 0;
  const pending = [root];
  for (let path = pending.pop(); path !== undefined; path = pending.pop()) {
    const listing = t.get(path) ?? { error: 'No such file or directory (os error 2)' };
    if (!Array.isArray(listing)) {
      if (path === root) throw new Error(`${root} could not be read: ${listing.error}`);
      unreadableFolders += 1;
      continue;
    }
    progress.folders += 1;
    for (const entry of listing) {
      if (entry.kind === 'folder') pending.push(`${path}/${entry.name}`);
      if (entry.kind !== 'file') continue;
      progress.files += 1;
      progress.bytes += entry.size;
      const extension = extensionOf(entry.name);
      const t0 = totals.get(extension) ?? { extension, files: 0, bytes: 0 };
      totals.set(extension, { extension, files: t0.files + 1, bytes: t0.bytes + entry.size });
    }
    report({ ...progress });
  }
  return {
    root,
    files: progress.files,
    totalBytes: progress.bytes,
    unreadableFolders,
    extensions: [...totals.values()].sort(byDisplayOrder),
    elapsedMs: 0,
  };
}

/** What a test reaches through `window.__mockBridge`. */
export interface MockHandle {
  /** Makes calls to `command` wait until `release(command)`. */
  hold(command: string): void;
  release(command: string): void;
  /** Every path passed to scan_folder, in order. */
  readonly scans: string[];
}

declare global {
  interface Window {
    __mockBridge?: MockHandle;
  }
}

export class Mock {
  private readonly tree: Tree;
  private readonly cancelPick: boolean;
  private readonly failing: string | null;
  private readonly listeners = new Set<(p: ScanProgress) => void>();
  private readonly held = new Map<string, { promise: Promise<void>; open: () => void }>();
  private readonly scans: string[] = [];

  constructor(search: string) {
    const params = new URLSearchParams(search);
    this.tree = tree(params.get('tree') ?? 'photos');
    this.cancelPick = params.get('pick') === 'cancel';
    this.failing = params.get('fail');
  }

  get handle(): MockHandle {
    return {
      hold: (command) => {
        let open = (): void => undefined;
        const promise = new Promise<void>((resolve) => {
          open = resolve;
        });
        this.held.set(command, { promise, open });
      },
      release: (command) => {
        this.held.get(command)?.open();
        this.held.delete(command);
      },
      scans: this.scans,
    };
  }

  private async gate(command: string): Promise<void> {
    await this.held.get(command)?.promise;
    if (this.failing === command) throw new Error(`Mock failure: ${command}`);
  }

  async appInfo(): Promise<AppInfo> {
    await this.gate('app_info');
    return { version: MOCK_VERSION };
  }

  pickFolder(): string | null {
    return this.cancelPick ? null : MOCK_ROOT;
  }

  /** Reports progress at once, then answers when the command's gate opens. */
  async scanFolder(path: string): Promise<ScanSummary> {
    this.scans.push(path);
    const summary = walk(this.tree, path, (p) => {
      for (const cb of this.listeners) cb(p);
    });
    await this.gate('scan_folder');
    return summary;
  }

  onProgress(cb: (p: ScanProgress) => void): () => void {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }
}
