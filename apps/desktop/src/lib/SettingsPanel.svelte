<script lang="ts">
  // The settings: the vocabulary file (the glossary that steers
  // transcription), shown as its path with Browse… (the native open panel)
  // and Open in Text Editor (which creates it empty when missing); then one
  // switch, for summaries by the local model. It shows no status about
  // Ollama; a summary simply replaces the excerpt when it lands.
  import type { Settings } from './types';

  interface Props {
    settings: Settings;
    open: boolean;
    error: string | null;
    onsummaries: (on: boolean) => void;
    onbrowse: () => void;
    onopenvocabulary: () => void;
  }

  const { settings, open, error, onsummaries, onbrowse, onopenvocabulary }: Props = $props();
</script>

<section id="settings" class="settings" aria-label="Settings" hidden={!open}>
  <div class="vocabulary" role="group" aria-label="Vocabulary file">
    <span class="caption" aria-hidden="true">Vocabulary file</span>
    <div class="row">
      <input readonly aria-label="Vocabulary file" value={settings.vocabulary} title={settings.vocabulary} />
      <button type="button" onclick={onbrowse}>Browse…</button>
      <button type="button" onclick={onopenvocabulary}>Open in Text Editor</button>
    </div>
  </div>
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
  {#if error !== null}
    <p class="error" role="alert">{error}</p>
  {/if}
</section>
