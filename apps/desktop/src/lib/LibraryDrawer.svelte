<script lang="ts">
  // The recording library: past dictations, newest first, each its start
  // and a one-line summary. Selecting one shows it in the main pane.
  import type { LibraryEntry } from './types';

  interface Props {
    entries: LibraryEntry[];
    open: boolean;
    selected: string | null;
    error: string | null;
    onselect: (entry: LibraryEntry) => void;
  }

  const { entries, open, selected, error, onselect }: Props = $props();
</script>

<nav id="library" class="library" aria-label="Recordings" hidden={!open}>
  {#if error !== null}
    <p class="error" role="alert">Could not read the library: {error}</p>
  {:else if entries.length === 0}
    <p class="empty">No recordings yet.</p>
  {/if}
  <ul>
    {#each entries as entry (entry.id)}
      <li>
        <button
          type="button"
          aria-current={selected === entry.id ? 'true' : undefined}
          onclick={() => {
            onselect(entry);
          }}
        >
          <span class="entry-title">{entry.title}</span>
          <span class="entry-summary">{entry.summary}</span>
        </button>
      </li>
    {/each}
  </ul>
</nav>
