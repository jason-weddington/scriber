<script lang="ts">
  // The dictation screen: New, Record, Pause, Resume, Done; the transcript
  // appears in an editable box and is already on the clipboard. Every rule
  // lives in the core; this file shows its snapshot and sends the user's
  // actions. Patterns: docs/tauri-design-notes.md.
  import { onMount } from 'svelte';

  import { act, appVersion, dictationState, errorText, onDictationChanged } from './lib/bridge';
  import { allowed, formatElapsed, statusText } from './lib/format';
  import type { Action, Snapshot } from './lib/types';

  let snapshot = $state<Snapshot | null>(null);
  let loadError = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let busy = $state(false);
  let draft = $state('');
  let version = $state('');
  // While recording, the running segment's time is counted here: the core
  // only knows the length of segments that are closed.
  let recordingSince = $state<number | null>(null);
  let now = $state(Date.now());

  const elapsed = $derived(
    snapshot === null ? 0 : snapshot.recordedMs + (recordingSince === null ? 0 : Math.max(0, now - recordingSince)),
  );

  /** Takes a snapshot unless a newer one has already arrived (commands and events race). */
  function accept(next: Snapshot): void {
    if (snapshot !== null && next.revision < snapshot.revision) return;
    if (next.phase === 'recording') recordingSince ??= Date.now();
    else recordingSince = null;
    if (next.phase === 'finished' && (snapshot?.phase !== 'finished' || snapshot.text !== next.text)) draft = next.text;
    snapshot = next;
  }

  async function load(): Promise<void> {
    loadError = null;
    try {
      accept(await dictationState());
    } catch (e) {
      loadError = errorText(e);
    }
  }

  async function run(action: Action): Promise<void> {
    busy = true;
    try {
      accept(await act(action, action === 'copy' ? draft : ''));
      actionError = null;
    } catch (e) {
      actionError = errorText(e);
    } finally {
      busy = false;
    }
  }

  // The root component lives as long as the window, so the subscription is
  // never undone.
  onMount(() => {
    void onDictationChanged(accept);
    void load();
    // The footer stays empty when the version cannot be read.
    appVersion().then(
      (v) => {
        version = v;
      },
      () => undefined,
    );
  });

  $effect(() => {
    if (recordingSince === null) return;
    const timer = setInterval(() => {
      now = Date.now();
    }, 250);
    return () => {
      clearInterval(timer);
    };
  });

  function can(action: Action): boolean {
    if (snapshot === null || busy) return false;
    if ((action === 'record' || action === 'resume') && snapshot.modelError !== null) return false;
    return allowed(snapshot.phase, action);
  }
</script>

<div class="window">
  <header class="band" data-tauri-drag-region>
    <h1 data-tauri-drag-region>Dictation</h1>
    {#if snapshot !== null && snapshot.phase !== 'idle'}
      <span class="time" class:live={snapshot.phase === 'recording'} title="Recorded time" data-tauri-drag-region>
        <span class="visually-hidden">Recorded&nbsp;</span>{formatElapsed(elapsed)}
      </span>
    {/if}
    <button type="button" onclick={() => run('new')} disabled={!can('new')}>New</button>
  </header>

  <main>
    {#if loadError !== null}
      <p class="error" role="alert">Could not read the session: {loadError}</p>
      <button type="button" onclick={load}>Try again</button>
    {:else if snapshot !== null}
      {#if snapshot.modelError !== null}
        <p class="error" role="alert">{snapshot.modelError}</p>
      {/if}
      <p class="status" aria-live="polite">{statusText(snapshot)}</p>
      {#if snapshot.phase === 'failed' && snapshot.error !== null}
        <p class="error" role="alert">{snapshot.error}</p>
      {/if}
      {#if actionError !== null}
        <p class="error" role="alert">{actionError}</p>
      {/if}

      <div class="controls">
        <button type="button" class="primary" onclick={() => run('record')} disabled={!can('record')}>Record</button>
        <button type="button" onclick={() => run('pause')} disabled={!can('pause')}>Pause</button>
        <button type="button" class="primary" onclick={() => run('resume')} disabled={!can('resume')}>Resume</button>
        <button type="button" onclick={() => run('done')} disabled={!can('done')}>Done</button>
        <button type="button" class="quiet" onclick={() => run('discard')} disabled={!can('discard')}>Discard</button>
        {#if snapshot.phase === 'failed'}
          <button type="button" class="primary" onclick={() => run('retry')} disabled={!can('retry')}>Retry</button>
        {/if}
      </div>

      {#if snapshot.phase === 'finished'}
        <div class="transcript">
          <div class="transcript-head">
            <label for="transcript">Transcript</label>
            <button type="button" class="primary" onclick={() => run('copy')} disabled={!can('copy') || draft === ''}>
              Copy
            </button>
          </div>
          <textarea id="transcript" bind:value={draft} spellcheck="true"></textarea>
        </div>
      {/if}
    {/if}
  </main>

  {#if version !== ''}
    <footer>Version {version}</footer>
  {/if}
</div>
