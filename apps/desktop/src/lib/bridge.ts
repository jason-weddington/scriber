// The bridge is the ONLY file that imports Tauri (scripts/check-architecture.sh
// and the eslint config both enforce it). Everything else in the frontend
// talks to the Rust core through the functions here, so the seam between the
// webview and the core has a single address. The command names, argument
// shapes and events are the "Bridge contract" in docs/design.md, and
// tests/e2e/contract.spec.ts checks them against src-tauri/src/lib.rs.
//
// Outside Tauri (a plain browser, Playwright) every function is answered by
// the deterministic mock in mock.ts, which identifies itself: <html> carries
// data-bridge="mock" and app_info reports version 0.0.0-mock. A green browser
// test can therefore never be read as proof that the Tauri path works.
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';

import { Mock } from './mock';
import type { AppInfo, ScanProgress, ScanSummary } from './types';

/** The event src-tauri emits while a scan runs (SCAN_PROGRESS_EVENT in lib.rs). */
export const SCAN_PROGRESS_EVENT = 'scan-progress';

/** True inside the Tauri webview rather than a plain browser. */
export function underTauri(): boolean {
  return '__TAURI_INTERNALS__' in window;
}

let mockInstance: Mock | null = null;

function mock(): Mock {
  if (mockInstance === null) {
    mockInstance = new Mock(location.search);
    window.__mockBridge = mockInstance.handle;
    document.documentElement.dataset.bridge = 'mock';
  }
  return mockInstance;
}

/** Marks <html> with the backend in use, so a test or a person can tell. */
export function identify(): void {
  if (underTauri()) document.documentElement.dataset.bridge = 'tauri';
  else mock();
}

/** A command failure as one readable sentence (Tauri rejects with a bare string). */
export function errorText(e: unknown): string {
  if (e instanceof Error) return e.message;
  return typeof e === 'string' ? e : JSON.stringify(e);
}

export async function appInfo(): Promise<AppInfo> {
  if (!underTauri()) return mock().appInfo();
  return invoke<AppInfo>('app_info');
}

/** The folder picker. Resolves to null when the user cancels. */
export async function pickFolder(): Promise<string | null> {
  if (!underTauri()) return mock().pickFolder();
  const picked = await open({ directory: true, multiple: false, title: 'Choose a folder to scan' });
  return typeof picked === 'string' ? picked : null;
}

export async function scanFolder(path: string): Promise<ScanSummary> {
  if (!underTauri()) return mock().scanFolder(path);
  return invoke<ScanSummary>('scan_folder', { path });
}

/** Subscribes to scan progress. Resolves to the unsubscribe function. */
export async function onScanProgress(cb: (progress: ScanProgress) => void): Promise<() => void> {
  if (!underTauri()) return mock().onProgress(cb);
  return listen<ScanProgress>(SCAN_PROGRESS_EVENT, (e) => {
    cb(e.payload);
  });
}
