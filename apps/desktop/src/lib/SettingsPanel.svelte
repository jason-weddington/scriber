<script lang="ts">
  // The settings: the vocabulary file (the glossary that steers
  // transcription), shown as its path with Browse… (the native open panel)
  // and Open in Text Editor (which creates it empty when missing); then two
  // switches, each followed by its model: post-processing of each transcript
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
  // excerpt when it lands.
  import type { Settings } from './types';

  interface Props {
    settings: Settings;
    open: boolean;
    error: string | null;
    models: string[] | null;
    ollamaDown: boolean;
    ollamaReason: string;
    onsummaries: (on: boolean) => void;
    oncleanup: (on: boolean) => void;
    onsummarymodel: (name: string) => void;
    oncleanupmodel: (name: string) => void;
    onbrowse: () => void;
    onopenvocabulary: () => void;
    onclose: () => void;
  }

  const {
    settings,
    open,
    error,
    models,
    ollamaDown,
    ollamaReason,
    onsummaries,
    oncleanup,
    onsummarymodel,
    oncleanupmodel,
    onbrowse,
    onopenvocabulary,
    onclose,
  }: Props = $props();

  let dialog = $state<HTMLDialogElement>();
  let path = $state<HTMLInputElement>();
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
      <input readonly bind:this={path} aria-label="Vocabulary file" value={settings.vocabulary} title={settings.vocabulary} />
      <button type="button" onclick={onbrowse}>Browse…</button>
      <button type="button" onclick={onopenvocabulary}>Open in Text Editor</button>
    </div>
  </div>
  <label class="switch">
    <input
      type="checkbox"
      role="switch"
      checked={settings.cleanup}
      onchange={(e) => {
        oncleanup(e.currentTarget.checked);
      }}
    />
    Enable post-processing with local LLM
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
    Summarize recordings with local AI (Ollama)
  </label>
  <label for="summary-model">Summary model</label>
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
  {#if ollamaDown}
    <p class="note" title={ollamaReason}>Local AI needs Ollama. Start Ollama, then reopen Settings.</p>
  {/if}
  {#if error !== null}
    <p class="error" role="alert">{error}</p>
  {/if}
  </div>
</dialog>
