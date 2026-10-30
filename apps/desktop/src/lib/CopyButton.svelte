<script lang="ts">
  // A quiet icon button: two sheets of paper, a green check for 2 s after a successful copy.
  const { disabled = false, copy }: { disabled?: boolean; copy: () => Promise<boolean> } = $props();

  const COPIED_MS = 2000;
  let copied = $state(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let clicks = 0;

  $effect(() => () => {
    clicks++;
    clearTimeout(timer);
  });

  async function click(): Promise<void> {
    const mine = ++clicks;
    clearTimeout(timer);
    copied = false;
    const ok = await copy();
    if (!ok || mine !== clicks) return;
    copied = true;
    timer = setTimeout(() => {
      copied = false;
    }, COPIED_MS);
  }
</script>

<button type="button" class="copy" class:copied aria-label="Copy" title="Copy" {disabled} onclick={click}>
  {#if copied}
    <svg aria-hidden="true" viewBox="0 0 16 16" width="16" height="16">
      <path d="M3 8.5l3.5 3.5L13 4.5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
    </svg>
  {:else}
    <svg aria-hidden="true" viewBox="0 0 16 16" width="16" height="16">
      <rect x="5.5" y="5.5" width="9" height="9" rx="1.5" fill="none" stroke="currentColor" />
      <path d="M10.5 3a1.5 1.5 0 0 0-1.5-1.5H3A1.5 1.5 0 0 0 1.5 3v6A1.5 1.5 0 0 0 3 10.5" fill="none" stroke="currentColor" />
    </svg>
  {/if}
</button>
