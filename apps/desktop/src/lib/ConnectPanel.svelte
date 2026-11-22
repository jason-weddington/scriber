<script lang="ts">
  // Native modal dialog for configuring the web server URL and shared token.
  // Pre-fills the effective origin and leaves the token blank on every open;
  // dismissals always clear the token draft without mutating storage or retrying.
  import { untrack } from 'svelte';

  import type { WebConnectionConfig } from './bridge';

  interface Props {
    open: boolean;
    effectiveOrigin: string;
    feedback: string | null;
    connecting: boolean;
    canRetry: boolean;
    onsubmit: (config: WebConnectionConfig) => void;
    onretry: () => void;
    oncancel: () => void;
  }

  const {
    open,
    effectiveOrigin,
    feedback,
    connecting,
    canRetry,
    onsubmit,
    onretry,
    oncancel,
  }: Props = $props();

  let dialog = $state<HTMLDialogElement>();
  let urlInput = $state<HTMLInputElement>();
  let endpointDraft = $state('');
  let tokenDraft = $state('');
  let pressedBackdrop = false;
  let closingProgrammatically = false;
  let wasOpen = false;

  $effect(() => {
    if (open) {
      if (!wasOpen) {
        wasOpen = true;
        endpointDraft = untrack(() => effectiveOrigin);
        tokenDraft = '';
        if (dialog !== undefined && !dialog.open) {
          dialog.showModal();
        }
        urlInput?.focus();
      }
    } else if (wasOpen) {
      wasOpen = false;
      tokenDraft = '';
      if (dialog?.open) {
        closingProgrammatically = true;
        dialog.close();
      }
    }
  });

  function handleDialogClose(): void {
    tokenDraft = '';
    if (closingProgrammatically) {
      closingProgrammatically = false;
      return;
    }
    wasOpen = false;
    oncancel();
  }

  function handleSubmit(e: SubmitEvent): void {
    e.preventDefault();
    onsubmit({ endpoint: endpointDraft, token: tokenDraft });
  }
</script>

<dialog
  id="connect"
  class="settings connect"
  aria-labelledby="connect-title"
  bind:this={dialog}
  onclose={handleDialogClose}
  onpointerdown={(e) => {
    pressedBackdrop = e.target === e.currentTarget;
  }}
  onclick={(e) => {
    if (pressedBackdrop && e.target === e.currentTarget) e.currentTarget.close();
  }}
>
  <div class="settings-head">
    <h2 id="connect-title">Connect</h2>
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
  <form class="settings-body" novalidate onsubmit={handleSubmit}>
    <div class="connect-field">
      <label for="connect-endpoint">Server URL</label>
      <input
        id="connect-endpoint"
        type="text"
        inputmode="url"
        autocomplete="url"
        spellcheck="false"
        bind:this={urlInput}
        bind:value={endpointDraft}
      />
    </div>
    <div class="connect-field">
      <label for="connect-token">Shared token</label>
      <input
        id="connect-token"
        type="password"
        autocomplete="off"
        spellcheck="false"
        bind:value={tokenDraft}
      />
    </div>
    <div class="status-row">
      <p class="note" class:visually-hidden={feedback !== null} aria-live="polite">
        {connecting ? 'Connecting…' : ''}
      </p>
      {#if feedback !== null}
        <p class="error" role="alert" title={feedback}>{feedback}</p>
      {/if}
    </div>
    <div class="connect-actions">
      <button type="submit" class="primary">Connect</button>
      {#if canRetry}
        <button type="button" onclick={onretry}>Try again</button>
      {/if}
      <button
        type="button"
        class="quiet"
        onclick={() => {
          dialog?.close();
        }}
      >
        Cancel
      </button>
    </div>
  </form>
</dialog>

<style>
  .connect-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }

  .connect-field input {
    width: 100%;
    min-width: 0;
    padding: 6px 8px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--bg);
    color: var(--text);
    font: inherit;
    user-select: text;
  }

  .connect-field input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .connect-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin-top: 4px;
  }

  .connect-actions .quiet {
    margin-left: auto;
  }
</style>
