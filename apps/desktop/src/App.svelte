<script lang="ts">
  // The dictation screen: New, Record, Pause, Resume, Done; the transcript
  // appears in an editable box and is already on the clipboard. Every rule
  // lives in the core; this file shows its snapshot and sends the user's
  // actions. Patterns: docs/tauri-design-notes.md.
  import { onMount } from 'svelte';

  import { act, appVersion, dictationState, errorText, onDictationChanged, onLevel } from './lib/bridge';
  import {
    allowed,
    bars,
    barCount,
    downloadText,
    formatElapsed,
    percent,
    pushLevel,
    shortcut,
    SHORTCUTS,
    statusText,
  } from './lib/format';
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
  // The input levels the waveform draws, oldest first. Kept while paused,
  // cleared once the session leaves recording and paused.
  let levels = $state<number[]>([]);
  // The waveform's width in px, which sets how many bars fit.
  let waveWidth = $state(0);
  // Set when a Space keydown ran a shortcut, so its keyup cannot also click
  // a focused button.
  let swallowSpaceUp = false;
  // Whether the focused element got its focus from a click rather than the
  // keyboard. (Chromium's :focus-visible cannot tell: any key press makes the
  // focused element match it.)
  let pointerDown = false;
  let focusFromPointer = false;
  // Where Space and Return type rather than act.
  const TEXT_FIELDS = 'textarea, select, input:not([type=button], [type=checkbox], [type=radio], [type=submit], [type=reset])';

  const elapsed = $derived(
    snapshot === null ? 0 : snapshot.recordedMs + (recordingSince === null ? 0 : Math.max(0, now - recordingSince)),
  );
  const metering = $derived(snapshot?.phase === 'recording' || snapshot?.phase === 'paused');
  const latest = $derived(levels.at(-1) ?? 0);

  /** Takes a snapshot unless a newer one has already arrived (commands and events race). */
  function accept(next: Snapshot): void {
    if (snapshot !== null && next.revision < snapshot.revision) return;
    if (next.phase === 'recording') recordingSince ??= Date.now();
    else recordingSince = null;
    if (next.phase !== 'recording' && next.phase !== 'paused') levels = [];
    if (next.phase === 'finished' && (snapshot?.phase !== 'finished' || snapshot.text !== next.text)) draft = next.text;
    snapshot = next;
  }

  /** A level from the microphone; one that arrives after Pause is dropped, so the waveform freezes. */
  function hear(level: number): void {
    if (snapshot?.phase === 'recording') levels = pushLevel(levels, level);
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

  // The root component lives as long as the window, so the subscriptions are
  // never undone.
  onMount(() => {
    void onDictationChanged(accept);
    void onLevel(hear);
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

  // A focused button left disabled once an action settles (Record, once
  // recording) would keep the focus and swallow every key after it,
  // shortcuts included; hand the focus back to the window instead.
  $effect(() => {
    if (!busy && snapshot !== null) {
      const focused = document.activeElement;
      if (focused instanceof HTMLButtonElement && focused.disabled) focused.blur();
    }
  });

  function can(action: Action): boolean {
    if (snapshot === null || busy) return false;
    if (action === 'downloadModel') return snapshot.modelError !== null && snapshot.download === null;
    const modelReady = snapshot.modelError === null && snapshot.download === null;
    if ((action === 'record' || action === 'resume') && !modelReady) return false;
    return allowed(snapshot.phase, action);
  }

  /** Where Space and Return keep their own meaning: text fields, and a button reached with the keyboard (Tab). */
  function ownsKeys(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false;
    if (target.isContentEditable || target.matches(TEXT_FIELDS)) return true;
    return target instanceof HTMLButtonElement && !focusFromPointer;
  }

  // In-window shortcuts: ⌘N is New, Space is Record, Pause and Resume, Return
  // is Done. A key whose action the phase does not allow does nothing here.
  // Handling one cancels its default, so a button that merely has focus from
  // a click is not clicked as well; a button reached with Tab keeps Space and
  // Return for itself. A held key's repeats are swallowed.
  function onKeydown(e: KeyboardEvent): void {
    if (e.key === 'Tab') pointerDown = false;
    if (snapshot === null) return;
    const action = shortcut(e, snapshot.phase, ownsKeys(e.target));
    if (action === null) return;
    e.preventDefault();
    swallowSpaceUp = e.key === ' ';
    if (!e.repeat && can(action)) void run(action);
  }

  function onKeyup(e: KeyboardEvent): void {
    if (e.key !== ' ' || !swallowSpaceUp) return;
    swallowSpaceUp = false;
    e.preventDefault();
  }
</script>

<svelte:window
  onkeydown={onKeydown}
  onkeyup={onKeyup}
  onpointerdown={() => {
    pointerDown = true;
  }}
  onfocusin={() => {
    focusFromPointer = pointerDown;
    pointerDown = false;
  }}
/>

<div class="window">
  <header class="band" data-tauri-drag-region>
    <h1 data-tauri-drag-region>Dictation</h1>
    {#if snapshot !== null && snapshot.phase !== 'idle'}
      <span class="time" class:live={snapshot.phase === 'recording'} title="Recorded time" data-tauri-drag-region>
        <span class="visually-hidden">Recorded&nbsp;</span>{formatElapsed(elapsed)}
      </span>
    {/if}
    <button type="button" onclick={() => run('new')} disabled={!can('new')} aria-keyshortcuts={SHORTCUTS.new?.aria}>
      New<kbd aria-hidden="true">{SHORTCUTS.new?.hint}</kbd>
    </button>
  </header>

  <main>
    {#if loadError !== null}
      <p class="error" role="alert">Could not read the session: {loadError}</p>
      <button type="button" onclick={load}>Try again</button>
    {:else if snapshot !== null}
      {#if snapshot.download !== null}
        <div class="download">
          <p class="download-text" id="download-text">{downloadText(snapshot.download)}</p>
          <progress max="100" value={percent(snapshot.download)} aria-labelledby="download-text"></progress>
        </div>
      {/if}
      {#if snapshot.modelError !== null}
        <div class="model-error">
          <p class="error" role="alert">{snapshot.modelError}</p>
          <button type="button" onclick={() => run('downloadModel')} disabled={!can('downloadModel')}>
            Retry download
          </button>
        </div>
      {/if}
      <p class="status" aria-live="polite">{statusText(snapshot)}</p>
      {#if snapshot.phase === 'failed' && snapshot.error !== null}
        <p class="error" role="alert">{snapshot.error}</p>
      {/if}
      {#if actionError !== null}
        <p class="error" role="alert">{actionError}</p>
      {/if}

      {#if metering}
        <div class="meter" class:frozen={snapshot.phase === 'paused'}>
          <div class="wave" aria-hidden="true" data-count={levels.length} data-slots={barCount(waveWidth)} bind:clientWidth={waveWidth}>
            {#each bars(levels, barCount(waveWidth)) as level, i (i)}
              <span class="bar" style:--level={level}></span>
            {/each}
          </div>
          <div class="level" aria-hidden="true">
            <span class="level-fill" style:--level={latest}></span>
          </div>
          <span class="visually-hidden">
            {snapshot.phase === 'paused'
              ? 'The microphone is off while paused.'
              : latest > 0
                ? 'The microphone is picking up sound.'
                : 'Listening for sound from the microphone.'}
          </span>
        </div>
      {/if}

      <div class="controls">
        <button
          type="button"
          class="primary"
          onclick={() => run('record')}
          disabled={!can('record')}
          aria-keyshortcuts={SHORTCUTS.record?.aria}>Record<kbd aria-hidden="true">{SHORTCUTS.record?.hint}</kbd></button
        >
        <button type="button" onclick={() => run('pause')} disabled={!can('pause')} aria-keyshortcuts={SHORTCUTS.pause?.aria}
          >Pause<kbd aria-hidden="true">{SHORTCUTS.pause?.hint}</kbd></button
        >
        <button
          type="button"
          class="primary"
          onclick={() => run('resume')}
          disabled={!can('resume')}
          aria-keyshortcuts={SHORTCUTS.resume?.aria}>Resume<kbd aria-hidden="true">{SHORTCUTS.resume?.hint}</kbd></button
        >
        <button type="button" onclick={() => run('done')} disabled={!can('done')} aria-keyshortcuts={SHORTCUTS.done?.aria}
          >Done<kbd aria-hidden="true">{SHORTCUTS.done?.hint}</kbd></button
        >
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
