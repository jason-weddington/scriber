<script lang="ts">
  // The recording library: past dictations, newest first, grouped by day.
  // Each row leads with its summary (up to two lines) and the local time
  // beneath it. Selecting one shows it in the main pane. The drawer's right
  // edge is a handle: drag it, or use the arrow keys, to resize.
  import {
    clampLibraryWidth,
    groupByDay,
    LIBRARY_WIDTH_MAX,
    LIBRARY_WIDTH_MIN,
    LIBRARY_WIDTH_STEP,
    rowSummary,
    timeLabel,
  } from './format';
  import type { LibraryEntry } from './types';

  interface Props {
    entries: LibraryEntry[];
    open: boolean;
    selected: string | null;
    error: string | null;
    width: number;
    onselect: (entry: LibraryEntry) => void;
    /** The width changed (live, not saved). */
    onresize: (width: number) => void;
    /** A drag or key press ended with a new width (save it). */
    onresizeend: () => void;
  }

  const { entries, open, selected, error, width, onselect, onresize, onresizeend }: Props = $props();

  // The day groups relabel when the clock passes midnight: `tick` moves every
  // minute, and any change to `entries` re-reads the clock too.
  let tick = $state(0);
  const groups = $derived.by(() => {
    // Reading `tick` makes the clock re-read each minute (it is never negative).
    return groupByDay(entries, new Date(Date.now() + Math.min(0, tick)));
  });

  $effect(() => {
    const timer = setInterval(() => {
      tick += 1;
    }, 60_000);
    return () => { clearInterval(timer); };
  });

  let nav = $state<HTMLElement>();
  let dragging = false;
  let startX = 0;
  let startWidth = 0;
  let startValue = 0;
  let keyStartValue: number | null = null;

  function onpointerdown(e: PointerEvent): void {
    if (e.button !== 0) return;
    startX = e.clientX;
    startWidth = (nav as HTMLElement).getBoundingClientRect().width;
    startValue = width;
    dragging = true;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function onpointermove(e: PointerEvent): void {
    if (!dragging) return;
    onresize(clampLibraryWidth(startWidth + e.clientX - startX));
  }

  function onlostpointercapture(): void {
    dragging = false;
    if (width !== startValue) onresizeend();
  }

  function onkeydown(e: KeyboardEvent): void {
    const rendered = Math.round((nav as HTMLElement).getBoundingClientRect().width);
    let next: number;
    if (e.key === 'ArrowRight') next = rendered + LIBRARY_WIDTH_STEP;
    else if (e.key === 'ArrowLeft') next = rendered - LIBRARY_WIDTH_STEP;
    else if (e.key === 'Home') next = LIBRARY_WIDTH_MIN;
    else if (e.key === 'End') next = LIBRARY_WIDTH_MAX;
    else return;
    e.preventDefault();
    keyStartValue ??= width;
    const clamped = clampLibraryWidth(next);
    if (clamped !== width) onresize(clamped);
  }

  function finishKeys(): void {
    if (keyStartValue === null) return;
    const before = keyStartValue;
    keyStartValue = null;
    if (width !== before) onresizeend();
  }

  function onkeyup(e: KeyboardEvent): void {
    if (['ArrowRight', 'ArrowLeft', 'Home', 'End'].includes(e.key)) finishKeys();
  }
</script>

<nav id="library" class="library" aria-label="Recordings" hidden={!open} style:width="{width}px" bind:this={nav}>
  {#if error !== null}
    <p class="error" role="alert">Could not read the library: {error}</p>
  {:else if entries.length === 0}
    <p class="empty">No recordings yet.</p>
  {/if}
  <div class="library-scroll">
    {#each groups as group, i (group.label)}
      <p class="day" id="library-day-{i}">{group.label}</p>
      <ul aria-labelledby="library-day-{i}">
        {#each group.entries as entry (entry.id)}
          <li>
            <button
              type="button"
              aria-current={selected === entry.id ? 'true' : undefined}
              onclick={() => {
                onselect(entry);
              }}
            >
              <span class="entry-summary">{rowSummary(entry.summary)}</span>
              <span class="entry-time">{timeLabel(entry.title)}</span>
            </button>
          </li>
        {/each}
      </ul>
    {/each}
  </div>
  <!-- A focusable separator is a widget in WAI-ARIA (a window splitter), which
       Svelte's role table does not know, so it flags the tabindex and the
       handlers. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div
    class="library-handle"
    role="separator"
    tabindex="0"
    aria-orientation="vertical"
    aria-label="Resize library"
    aria-valuenow={width}
    aria-valuemin={LIBRARY_WIDTH_MIN}
    aria-valuemax={LIBRARY_WIDTH_MAX}
    {onpointerdown}
    {onpointermove}
    {onlostpointercapture}
    {onkeydown}
    {onkeyup}
    onblur={finishKeys}
  ></div>
</nav>
