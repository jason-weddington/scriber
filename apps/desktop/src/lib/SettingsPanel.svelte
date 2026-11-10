<script lang="ts">
  // The settings: the vocabulary file (the glossary that steers
  // transcription), the learned vocabulary file (the terms learning
  // remembers) and the speech model's folder (where the 1.6 GB model is
  // downloaded and read, so apps can share one copy), each shown as its name,
  // the full path as tooltip and accessible description, with
  // Browse… on all three, Open in Text Editor on the two text files (which
  // creates it empty when missing); then the
  // silence split, "Split into parts at a pause" (Off, 1.5 s, 2.5 s or 4 s; a
  // saved value outside those four is shown first, so it is never lost); then
  // the "Play a sound when the transcript is copied" switch, which turns the
  // menu bar's copy sound on and off; then two more switches, each followed by
  // its model: post-processing of each transcript
  // by the local model (glossary fixes and spoken self-corrections), and
  // summaries by the local model. The models are chosen from the ones Ollama
  // has installed, listed each time the modal opens (`models` is null while
  // the listing is in flight). While listing, and when Ollama cannot be
  // reached, each dropdown shows only its saved model and is disabled; once
  // listed, a saved model that is not installed is still shown, marked
  // "(not installed)". When Ollama is down one note says so, with the reason
  // on hover. The switches stay enabled in every state, so local AI can
  // always be turned off. Nothing else shows status about Ollama: a cleanup
  // that fails leaves Whisper's text, and a summary simply replaces the
  // excerpt when it lands. The modal ends with the app's version when it can
  // be read.
  import { captureShortcut, fileName, shortcutAria, shortcutGlyphs } from './format';
  import type { Settings } from './types';

  interface Props {
    settings: Settings;
    open: boolean;
    error: string | null;
    models: string[] | null;
    ollamaDown: boolean;
    ollamaReason: string;
    version: string;
    onsummaries: (on: boolean) => void;
    oncleanup: (on: boolean) => void;
    onsummarymodel: (name: string) => void;
    oncleanupmodel: (name: string) => void;
    onbrowse: () => void;
    onopenvocabulary: () => void;
    onbrowselearned: () => void;
    onopenlearned: () => void;
    onbrowsemodel: () => void;
    onsilencesplit: (ms: number) => void;
    onshortcut: (value: string) => void;
    onpauseshortcut: (value: string) => void;
    oncopysound: (on: boolean) => void;
    onclose: () => void;
  }

  const {
    settings,
    open,
    error,
    models,
    ollamaDown,
    ollamaReason,
    version,
    onsummaries,
    oncleanup,
    onsummarymodel,
    oncleanupmodel,
    onbrowse,
    onopenvocabulary,
    onbrowselearned,
    onopenlearned,
    onbrowsemodel,
    onsilencesplit,
    onshortcut,
    onpauseshortcut,
    oncopysound,
    onclose,
  }: Props = $props();

  let dialog = $state<HTMLDialogElement>();
  let path = $state<HTMLInputElement>();
  // Which shortcut button is capturing the next key press, and why the last
  // press was refused.
  let armed = $state<'toggle' | 'pause' | null>(null);
  let refusal = $state<string | null>(null);
  // What each button shows: the saved shortcut as glyphs, or the capture's prompt.
  const shown = $derived(armed === 'toggle' ? 'Press a shortcut…' : shortcutGlyphs(settings.toggleShortcut));
  const pauseShown = $derived(armed === 'pause' ? 'Press a shortcut…' : settings.pauseShortcut === '' ? 'None' : shortcutGlyphs(settings.pauseShortcut));
  // Whether the press that led to a click began on the overlay, so that a
  // drag from inside the modal released on the overlay does not close it.
  let pressedBackdrop = false;

  $effect(() => {
    if (!open) return;
    dialog?.showModal();
    // The heading row comes first, but the settings themselves are what opens.
    path?.focus();
  });

  // The installed models, once listed; null while loading or when Ollama is down.
  const listed = $derived(models !== null && !ollamaDown ? models : null);

  // Each option carries its own `selected` (and its value through a spread):
  // Svelte compiles `<select value>` and `<option {value}>` to an inline
  // `?? ''` fallback that a string value never takes.

  /** A dropdown's options for the saved model `value`, as [value, text] pairs. */
  function options(value: string): [string, string][] {
    if (listed === null) return [[value, value]];
    const installed = listed.map((name): [string, string] => [name, name]);
    return value !== '' && !listed.includes(value) ? [[value, `${value} (not installed)`], ...installed] : installed;
  }

  /** The silence splits the window offers, in ms, as [value, text] pairs. */
  const SPLITS: [number, string][] = [
    [0, 'Off'],
    [1500, '1.5 s'],
    [2500, '2.5 s'],
    [4000, '4 s'],
  ];

  /** The split options for the saved `ms`: the four, after `ms` itself when it is none of them. */
  function splits(ms: number): [number, string][] {
    return SPLITS.some(([value]) => value === ms) ? SPLITS : [[ms, `${ms / 1000} s`], ...SPLITS];
  }
</script>

<dialog
  id="settings"
  class="settings"
  aria-labelledby="settings-title"
  bind:this={dialog}
  {onclose}
  onpointerdown={(e) => {
    pressedBackdrop = e.target === e.currentTarget;
  }}
  onclick={(e) => {
    if (pressedBackdrop && e.target === e.currentTarget) e.currentTarget.close();
  }}
>
  <div class="settings-head">
    <h2 id="settings-title">Settings</h2>
    <button
      type="button"
      class="icon quiet"
      aria-label="Close"
      onclick={() => {
        dialog?.close();
      }}
    >
      <svg aria-hidden="true" viewBox="0 0 16 16" width="14" height="14">
        <path fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" d="M3 3l10 10M13 3L3 13" />
      </svg>
    </button>
  </div>
  <div class="settings-body">
  <div class="vocabulary" role="group" aria-label="Vocabulary file">
    <span class="caption" aria-hidden="true">Vocabulary file</span>
    <div class="row">
      <input readonly bind:this={path} aria-label="Vocabulary file" value={fileName(settings.vocabulary)} title={settings.vocabulary} />
      <button type="button" onclick={onbrowse}>Browse…</button>
      <button type="button" onclick={onopenvocabulary}>Open in Text Editor</button>
    </div>
  </div>
  <div class="vocabulary" role="group" aria-label="Learned vocabulary file">
    <span class="caption" aria-hidden="true">Learned vocabulary file</span>
    <div class="row">
      <input readonly aria-label="Learned vocabulary file" value={fileName(settings.learnedVocabulary)} title={settings.learnedVocabulary} />
      <button type="button" onclick={onbrowselearned}>Browse…</button>
      <button type="button" onclick={onopenlearned}>Open in Text Editor</button>
    </div>
  </div>
  <div class="vocabulary" role="group" aria-label="Speech model folder">
    <span class="caption" aria-hidden="true">Speech model folder</span>
    <div class="row">
      <input readonly aria-label="Speech model folder" value={fileName(settings.modelFolder)} title={settings.modelFolder} />
      <button type="button" onclick={onbrowsemodel}>Browse…</button>
    </div>
  </div>
  <label for="silence-split">Split into parts at a pause</label>
  <select
    id="silence-split"
    onchange={(e) => {
      onsilencesplit(Number(e.currentTarget.value));
    }}
  >
    {#each splits(settings.silenceSplitMs) as [ms, text] (ms)}
      {@const value = String(ms)}
      <option {...{ value }} selected={ms === settings.silenceSplitMs}>{text}</option>
    {/each}
  </select>
  <div class="vocabulary" role="group" aria-label="Toggle dictation shortcut">
    <span class="caption" aria-hidden="true">Toggle dictation shortcut</span>
    <div class="row">
      <button
        type="button"
        aria-label={`Toggle dictation shortcut: ${shown}`}
        aria-keyshortcuts={shortcutAria(settings.toggleShortcut)}
        title={shortcutAria(settings.toggleShortcut)}
        onclick={() => {
          armed = 'toggle';
          refusal = null;
        }}
        onkeydown={(e) => {
          if (armed !== 'toggle') return;
          // Every key belongs to the capture: the modal must not see the
          // Escape that would close it, nor Space or Return that would click
          // the button again.
          e.preventDefault();
          const capture = captureShortcut(e);
          if (capture.kind === 'cancel') {
            armed = null;
            refusal = null;
          } else if (capture.kind === 'needsModifier') {
            refusal = 'A shortcut needs ⌥ or ⌘.';
          } else if (capture.kind === 'badKey') {
            refusal = 'That key cannot be a shortcut.';
          } else if (capture.value === settings.pauseShortcut) {
            refusal = 'That shortcut is the pause shortcut.';
          } else {
            armed = null;
            refusal = null;
            onshortcut(capture.value);
          }
        }}
      >
        {shown}
      </button>
    </div>
  </div>
  <div class="vocabulary" role="group" aria-label="Pause shortcut">
    <span class="caption" aria-hidden="true">Pause shortcut</span>
    <div class="row">
      <button
        type="button"
        aria-label={`Pause shortcut: ${pauseShown}`}
        aria-keyshortcuts={shortcutAria(settings.pauseShortcut) || undefined}
        title={shortcutAria(settings.pauseShortcut) || undefined}
        onclick={() => {
          armed = 'pause';
          refusal = null;
        }}
        onkeydown={(e) => {
          if (armed !== 'pause') return;
          e.preventDefault();
          const capture = captureShortcut(e);
          if (capture.kind === 'cancel') {
            armed = null;
            refusal = null;
          } else if (capture.kind === 'needsModifier') {
            refusal = 'A shortcut needs ⌥ or ⌘.';
          } else if (capture.kind === 'badKey') {
            refusal = 'That key cannot be a shortcut.';
          } else if (capture.value === settings.toggleShortcut) {
            refusal = 'That shortcut is the toggle shortcut.';
          } else {
            armed = null;
            refusal = null;
            onpauseshortcut(capture.value);
          }
        }}
      >
        {pauseShown}
      </button>
    </div>
  </div>
  <label class="switch">
    <input
      type="checkbox"
      role="switch"
      checked={settings.copySound}
      onchange={(e) => {
        oncopysound(e.currentTarget.checked);
      }}
    />
    Play a sound when the transcript is copied
  </label>
  <label class="switch">
    <input
      type="checkbox"
      role="switch"
      checked={settings.cleanup}
      onchange={(e) => {
        oncleanup(e.currentTarget.checked);
      }}
    />
    Enable post-processing with local AI (Ollama)
  </label>
  <label for="cleanup-model">Post-processing model</label>
  <select
    id="cleanup-model"
    disabled={listed === null}
    onchange={(e) => {
      oncleanupmodel(e.currentTarget.value);
    }}
  >
    {#each options(settings.cleanupModel) as [value, text] (value)}
      <option {...{ value }} selected={value === settings.cleanupModel}>{text}</option>
    {/each}
  </select>
  <label class="switch">
    <input
      type="checkbox"
      role="switch"
      checked={settings.summaries}
      onchange={(e) => {
        onsummaries(e.currentTarget.checked);
      }}
    />
    Generate library titles with local AI (Ollama)
  </label>
  <label for="summary-model">Title model</label>
  <select
    id="summary-model"
    disabled={listed === null}
    onchange={(e) => {
      onsummarymodel(e.currentTarget.value);
    }}
  >
    {#each options(settings.summaryModel) as [value, text] (value)}
      <option {...{ value }} selected={value === settings.summaryModel}>{text}</option>
    {/each}
  </select>
  <div class="status-row">
    <p class="note" class:visually-hidden={error !== null} title={ollamaDown ? ollamaReason : undefined}>{ollamaDown ? 'Local AI needs Ollama. Start Ollama, then reopen Settings.' : ''}</p>
    {#if error !== null || refusal !== null}
      <p class="error" role="alert" title={error ?? refusal}>{error ?? refusal}</p>
    {/if}
  </div>
  {#if version !== ''}
    <p class="version">Version {version}</p>
  {/if}
  </div>
</dialog>
