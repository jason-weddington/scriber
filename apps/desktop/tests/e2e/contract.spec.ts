import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { expect, test } from './fixtures';

import { open } from './helpers';

// The packaged app talks to Rust through bridge.ts's invoke and listen calls,
// which no browser run exercises (they answer from the mock). This stubs
// Tauri's internals in the page, calls every bridge function, and checks the
// command names and argument keys it sent against the #[tauri::command]
// signatures in src-tauri/src/lib.rs, which it reads as text. A command
// renamed, an argument added or an event renamed on one side fails here.

const LIB_RS = resolve(process.cwd(), 'src-tauri/src/lib.rs');

function camel(snake: string): string {
  return snake.replace(/_([a-z])/g, (_, c: string) => c.toUpperCase());
}

/** Splits a Rust parameter list on the commas that are not inside <...>. */
function splitParams(list: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let cur = '';
  for (const ch of list) {
    if (ch === '<') depth++;
    if (ch === '>') depth--;
    if (ch === ',' && depth === 0) {
      out.push(cur);
      cur = '';
    } else {
      cur += ch;
    }
  }
  out.push(cur);
  return out.map((p) => p.trim()).filter(Boolean);
}

/** Arguments Tauri injects itself; the webview never sends them. */
const INJECTED = /:\s*(State\s*<|AppHandle\b|Window\b|WebviewWindow\b)/;

/** command name -> the argument keys the webview must send (Tauri camel-cases them). */
function rustCommands(src: string): Map<string, string[]> {
  const commands = new Map<string, string[]>();
  const re = /#\[tauri::command\]\s*(?:#\[[^\]]*\]\s*)*pub(?:\(crate\))?\s+(?:async\s+)?fn\s+(\w+)\s*\(([\s\S]*?)\)\s*(?:->[^{]*)?\{/g;
  for (const m of src.matchAll(re)) {
    const [, name = '', params = ''] = m;
    const keys = splitParams(params)
      .filter((p) => !INJECTED.test(p))
      .map((p) => camel(p.split(':')[0]?.trim() ?? ''))
      .sort();
    commands.set(name, keys);
  }
  return commands;
}

function registered(src: string): string[] {
  const block = /generate_handler!\[([\s\S]*?)\]/.exec(src)?.[1] ?? '';
  return [...block.matchAll(/commands::(\w+)/g)].map((m) => m[1] ?? '').sort();
}

function constant(src: string, name: string): string {
  const m = new RegExp(`pub const ${name}: &str = "([^"]+)";`).exec(src);
  if (!m?.[1]) throw new Error(`${name} not found in lib.rs`);
  return m[1];
}

interface Call {
  fn: string;
  args: unknown[];
  command: string;
  sent: Record<string, unknown>;
}

// Each bridge function that invokes a command, with sample arguments, and what it must send.
const CALLS: Call[] = [
  { fn: 'dictationState', args: [], command: 'dictation_state', sent: {} },
  { fn: 'act', args: ['pause'], command: 'dictation_act', sent: { action: 'pause', text: '' } },
  { fn: 'act', args: ['copy', 'Edited.'], command: 'dictation_act', sent: { action: 'copy', text: 'Edited.' } },
  { fn: 'act', args: ['downloadModel'], command: 'dictation_act', sent: { action: 'downloadModel', text: '' } },
  { fn: 'libraryList', args: [], command: 'library_list', sent: {} },
  { fn: 'libraryGet', args: ['20261026-140309'], command: 'library_get', sent: { id: '20261026-140309' } },
  { fn: 'libraryCopy', args: ['20261026-140309'], command: 'library_copy', sent: { id: '20261026-140309' } },
  { fn: 'settingsGet', args: [], command: 'settings_get', sent: {} },
  {
    fn: 'settingsSet',
    args: [{ summaries: false, libraryOpen: true, vocabulary: '/data/vocabulary.txt', cleanup: true }],
    command: 'settings_set',
    sent: { settings: { summaries: false, libraryOpen: true, vocabulary: '/data/vocabulary.txt', cleanup: true } },
  },
  { fn: 'vocabularyOpen', args: [], command: 'vocabulary_open', sent: {} },
];

const SNAPSHOT = {
  revision: 3,
  phase: 'paused',
  recordedMs: 1500,
  segments: 1,
  pending: 1,
  text: '',
  error: null,
  notice: null,
  modelError: null,
  download: null,
  libraryRevision: 0,
};

test.describe('bridge.ts against the Rust command signatures', () => {
  const src = readFileSync(LIB_RS, 'utf8');
  const rust = rustCommands(src);

  test('the parser sees every command, and every command is registered with the handler', () => {
    expect(rust.size).toBeGreaterThanOrEqual(2);
    expect(registered(src)).toEqual([...rust.keys()].sort());
  });

  test('every bridge function sends the right command with exactly the Rust argument keys', async ({ page }) => {
    await open(page);
    const sent = await page.evaluate(async ([calls, snapshot]) => {
      const url = '/src/lib/bridge.ts';
      const bridge = (await import(/* @vite-ignore */ url)) as Record<string, (...a: unknown[]) => Promise<unknown>>;
      const log: { command: string; args: Record<string, unknown> | undefined }[] = [];
      const callbacks: ((e: unknown) => void)[] = [];
      // What the stubbed open panel answers: a file, then a cancel.
      let picked: string | null = '/picked/terms.txt';
      const w = window as unknown as { __TAURI_INTERNALS__?: unknown };
      w.__TAURI_INTERNALS__ = {
        invoke: (command: string, args?: Record<string, unknown>) => {
          log.push({ command, args });
          if (command === 'plugin:dialog|open') return Promise.resolve(picked);
          return Promise.resolve(command === 'plugin:app|version' ? '9.9.9' : null);
        },
        transformCallback: (cb: (e: unknown) => void) => {
          callbacks.push(cb);
          return callbacks.length;
        },
      };
      try {
        for (const c of calls) await bridge[c.fn]?.(...c.args);
        const chosen = await bridge.pickVocabularyFile?.('/data/vocabulary.txt');
        picked = null;
        const cancelled = await bridge.pickVocabularyFile?.('/data/vocabulary.txt');
        log.push({ command: 'picked', args: { chosen, cancelled } });
        const received: unknown[] = [];
        await bridge.onDictationChanged?.((p: unknown) => received.push(p));
        await bridge.onLevel?.((p: unknown) => received.push(p));
        // Tauri delivers an event as { event, id, payload }; the bridge hands on the payload.
        callbacks[0]?.({ event: 'dictation-changed', id: 1, payload: snapshot });
        callbacks[1]?.({ event: 'dictation-level', id: 2, payload: 0.5 });
        log.push({ command: 'received', args: { received } });
        const version = await bridge.appVersion?.();
        log.push({ command: 'version', args: { version } });
      } finally {
        delete w.__TAURI_INTERNALS__;
      }
      return log;
    }, [CALLS, SNAPSHOT] as const);

    const commandCalls = sent.filter((c) => !c.command.startsWith('plugin:') && !['version', 'received', 'picked'].includes(c.command));
    expect(commandCalls.map((c) => c.command)).toEqual(CALLS.map((c) => c.command));

    for (const [i, call] of CALLS.entries()) {
      const keys = rust.get(call.command);
      expect(keys, `${call.command} is not a #[tauri::command] in lib.rs`).toBeDefined();
      const args = commandCalls[i]?.args ?? {};
      expect(Object.keys(args).sort(), `${call.command}: argument keys`).toEqual(keys);
      expect(args, `${call.command}: argument values`).toEqual(call.sent);
    }

    // Nothing in Rust is left without a bridge function.
    expect([...rust.keys()].sort()).toEqual([...new Set(CALLS.map((c) => c.command))].sort());

    // The subscription listens for the event name the Rust worker emits.
    const listened = sent
      .filter((c) => c.command === 'plugin:event|listen')
      .map((c) => (c.args as { event: string }).event);
    expect(listened).toEqual([constant(src, 'DICTATION_EVENT'), constant(src, 'LEVEL_EVENT')]);

    expect(sent.find((c) => c.command === 'received')?.args).toEqual({ received: [SNAPSHOT, 0.5] });

    // The vocabulary file's open panel goes through the dialog plugin, a file or null back.
    const panels = sent.filter((c) => c.command === 'plugin:dialog|open');
    expect(panels).toHaveLength(2);
    expect(panels[0]?.args).toEqual({
      options: { multiple: false, directory: false, defaultPath: '/data/vocabulary.txt', filters: [{ name: 'Text', extensions: ['txt'] }] },
    });
    expect(sent.find((c) => c.command === 'picked')?.args).toEqual({ chosen: '/picked/terms.txt', cancelled: null });

    // The version comes from Tauri's own app plugin, passed through.
    expect(sent.some((c) => c.command === 'plugin:app|version')).toBe(true);
    expect(sent.at(-1)).toEqual({ command: 'version', args: { version: '9.9.9' } });
  });

  test('the stub is gone afterwards: the page is back on the mock', async ({ page }) => {
    await open(page);
    await expect(page.locator('html')).toHaveAttribute('data-bridge', 'mock');
    expect(await page.evaluate(() => '__TAURI_INTERNALS__' in window)).toBe(false);
  });
});
