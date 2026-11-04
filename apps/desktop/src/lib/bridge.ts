// The bridge is the ONLY file that imports Tauri (scripts/check-architecture.sh
// and the eslint config both enforce it). Everything else in the frontend
// talks to the Rust core through the functions here, so the seam between the
// webview and the core has a single address. The command names, argument
// shapes and events are the "Bridge contract" in docs/design.md, and
// tests/e2e/contract.spec.ts checks them against src-tauri/src/lib.rs.
//
// Outside Tauri (a plain browser, Playwright) every function is answered by
// the deterministic mock in mock.ts, which identifies itself: <html> carries
// data-bridge="mock" and the version reads 0.0.0-mock. A green browser test
// can therefore never be read as proof that the Tauri path works.
import { getVersion } from '@tauri-apps/api/app';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { open } from '@tauri-apps/plugin-dialog';

import { Mock } from './mock';
import type { Action, InputDevices, LibraryEntry, Settings, Snapshot } from './types';

/** The event src-tauri emits when a job changes the snapshot (DICTATION_EVENT in lib.rs). */
export const DICTATION_EVENT = 'dictation-changed';

/** The event src-tauri emits with the input level while recording (LEVEL_EVENT in lib.rs). */
export const LEVEL_EVENT = 'dictation-level';

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

/** The app's version, from its manifest. */
export async function appVersion(): Promise<string> {
  if (!underTauri()) return mock().version();
  return getVersion();
}

/** The session as it stands. */
export async function dictationState(): Promise<Snapshot> {
  if (!underTauri()) return mock().state();
  return invoke<Snapshot>('dictation_state');
}

/** One user action. `text` is the edited transcript for `copy`, ignored otherwise. */
export async function act(action: Action, text = ''): Promise<Snapshot> {
  if (!underTauri()) return mock().act(action, text);
  return invoke<Snapshot>('dictation_act', { action, text });
}

/** The recording library, newest first. */
export async function libraryList(): Promise<LibraryEntry[]> {
  if (!underTauri()) return mock().libraryList();
  return invoke<LibraryEntry[]>('library_list');
}

/** One library entry's transcript. */
export async function libraryGet(id: string): Promise<string> {
  if (!underTauri()) return mock().libraryGet(id);
  return invoke<string>('library_get', { id });
}

/** Puts one library entry's transcript on the clipboard. */
export async function libraryCopy(id: string): Promise<void> {
  if (!underTauri()) return mock().libraryCopy(id);
  return invoke<undefined>('library_copy', { id });
}

/** The user's settings. */
export async function settingsGet(): Promise<Settings> {
  if (!underTauri()) return mock().settingsGet();
  return invoke<Settings>('settings_get');
}

/** The installed Ollama models' names, sorted; rejects when Ollama cannot be reached. */
export async function ollamaModels(): Promise<string[]> {
  if (!underTauri()) return mock().ollamaModels();
  return invoke<string[]>('ollama_models');
}

/** The Mac's default input device and every other one, by name; rejects when they cannot be listed. */
export async function inputDevices(): Promise<InputDevices> {
  if (!underTauri()) return mock().inputDevices();
  return invoke<InputDevices>('input_devices');
}

/** Saves the user's settings; resolves to what was saved. */
export async function settingsSet(settings: Settings): Promise<Settings> {
  if (!underTauri()) return mock().settingsSet(settings);
  return invoke<Settings>('settings_set', { settings });
}

/** Opens the glossary in the default text editor, creating it empty first. */
export async function vocabularyOpen(): Promise<void> {
  if (!underTauri()) return mock().vocabularyOpen();
  return invoke<undefined>('vocabulary_open');
}

/** Opens the learned list in the default text editor, creating it empty first. */
export async function learnedVocabularyOpen(): Promise<void> {
  if (!underTauri()) return mock().learnedVocabularyOpen();
  return invoke<undefined>('learned_vocabulary_open');
}

/** The native open panel for a glossary file, starting at `current`; null when cancelled. */
export async function pickVocabularyFile(current: string): Promise<string | null> {
  if (!underTauri()) return mock().pickVocabularyFile(current);
  const r = await open({ multiple: false, directory: false, defaultPath: current, filters: [{ name: 'Text', extensions: ['txt'] }] });
  return typeof r === 'string' ? r : null;
}

/** Subscribes to background changes (a segment transcribed, the model's download). Resolves to the unsubscribe function. */
export async function onDictationChanged(cb: (snapshot: Snapshot) => void): Promise<() => void> {
  if (!underTauri()) return mock().onChange(cb);
  return listen<Snapshot>(DICTATION_EVENT, (e) => {
    cb(e.payload);
  });
}

/** Whether the window is in full screen (the traffic lights are then outside it). */
export async function windowFullscreen(): Promise<boolean> {
  if (!underTauri()) return mock().fullscreen();
  return getCurrentWindow().isFullscreen();
}

/**
 * Subscribes to full-screen changes. Tauri 2 has no such event, so each resize re-reads the
 * state; only the latest read's answer is kept (the read is async), and a failed read keeps
 * the last known state. Resolves to the unsubscribe function.
 */
export async function onFullscreenChanged(cb: (on: boolean) => void): Promise<() => void> {
  if (!underTauri()) return mock().onFullscreen(cb);
  const win = getCurrentWindow();
  let reads = 0;
  return win.onResized(() => {
    reads += 1;
    const mine = reads;
    win.isFullscreen().then(
      (on) => {
        if (mine === reads) cb(on);
      },
      () => undefined,
    );
  });
}

/** Subscribes to the input level (0 to 1, about 20 a second) while recording. Resolves to the unsubscribe function. */
export async function onLevel(cb: (level: number) => void): Promise<() => void> {
  if (!underTauri()) return mock().onLevel(cb);
  return listen<number>(LEVEL_EVENT, (e) => {
    cb(e.payload);
  });
}
