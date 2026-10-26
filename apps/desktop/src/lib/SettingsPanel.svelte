<script lang="ts">
  // The settings: one switch, for summaries by the local model. It shows no
  // status about Ollama; a summary simply replaces the excerpt when it lands.
  import type { Settings } from './types';

  interface Props {
    settings: Settings;
    open: boolean;
    error: string | null;
    onsummaries: (on: boolean) => void;
  }

  const { settings, open, error, onsummaries }: Props = $props();
</script>

<section id="settings" class="settings" aria-label="Settings" hidden={!open}>
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
