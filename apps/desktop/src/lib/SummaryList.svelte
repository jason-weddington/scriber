<script lang="ts">
  // One row per extension, largest first. The list is the scrolling element
  // (min-height: 0 in its flex column), and rows never shrink, so any number
  // of rows stays reachable: tests/e2e/scroll.spec.ts proves it.
  import { extensionLabel, formatBytes, plural } from './format';
  import type { ScanSummary } from './types';

  let { summary }: { summary: ScanSummary } = $props();

  const largest = $derived(summary.extensions[0]?.bytes ?? 0);

  function share(bytes: number): string {
    return largest === 0 ? '0%' : `${((bytes / largest) * 100).toFixed(1)}%`;
  }
</script>

{#if summary.extensions.length === 0}
  <p class="empty">This folder has no files.</p>
{:else}
  <ul class="extensions" aria-label="File types">
    {#each summary.extensions as row (row.extension)}
      <li>
        <span class="name" title={extensionLabel(row.extension)}>{extensionLabel(row.extension)}</span>
        <span class="count">{plural(row.files, 'file', 'files')}</span>
        <span class="size">{formatBytes(row.bytes)}</span>
        <span class="bar" aria-hidden="true"><span class="fill" style:width={share(row.bytes)}></span></span>
      </li>
    {/each}
  </ul>
{/if}
