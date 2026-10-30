<script lang="ts">
  // The microphone control: names the input device that is open (or that the
  // next Record or Resume will open) and, on right-click, click, or Return or
  // Space once focused, opens an in-page menu of the input devices. The menu
  // is DOM rather than a native menu, so src-tauri stays thin and the e2e
  // tests reach it against the mock (docs/design.md, "Microphone.").
  import { tick } from 'svelte';

  import { defaultMicrophoneLabel } from './format';
  import type { InputDevices } from './types';

  interface Props {
    /** The microphone to name, or null when none is known. */
    name: string | null;
    /** The last known list of input devices, or null before the first listing. */
    inputs: InputDevices | null;
    /** The saved choice: an `others` name, or '' for the Mac default. */
    saved: string;
    /** Why the latest listing failed, if it did. */
    error: string | null;
    /** Called each time the menu opens, to list the devices again. */
    onopen: () => void;
    /** Called with the choice: an `others` name, or '' for the Mac default. */
    onchoose: (value: string) => void;
  }

  let { name, inputs, saved, error, onopen, onchoose }: Props = $props();

  let open = $state(false);
  let trigger = $state<HTMLButtonElement>();
  let popover = $state<HTMLDivElement>();
  let menu = $state<HTMLDivElement>();
  // Set when the pointer opened the menu, so closing it does not leave the
  // trigger focused (a focused trigger would take Space from the shortcut).
  let viaPointer = false;
  // The popover's place, from the trigger's rect when it opens.
  let top = $state<string | null>(null);
  let bottom = $state<string | null>(null);
  let left = $state<string | null>(null);
  let maxHeight = $state<string | null>(null);

  const others = $derived(inputs?.others ?? []);
  const checked = $derived(others.includes(saved) ? saved : '');
  const defaultLabel = $derived(defaultMicrophoneLabel(inputs?.defaultDevice ?? null));

  function show(): void {
    if (open || trigger === undefined) return;
    const r = trigger.getBoundingClientRect();
    const above = r.top - 12;
    const below = innerHeight - r.bottom - 12;
    const space = above > below ? above : below;
    top = above > below ? null : `${String(r.bottom + 4)}px`;
    bottom = above > below ? `${String(innerHeight - r.top + 4)}px` : null;
    left = `${String(r.left)}px`;
    maxHeight = `${String(Math.min(200, space))}px`;
    open = true;
    onopen();
  }

  function close(): void {
    open = false;
    viaPointer = false;
  }

  /** Closes the menu after Escape or a choice, handing the focus back to the trigger for the keyboard. */
  function closeAndRefocus(): void {
    const pointer = viaPointer;
    close();
    if (pointer) trigger?.blur();
    else trigger?.focus();
  }

  function choose(value: string): void {
    closeAndRefocus();
    onchoose(value);
  }

  function items(): HTMLButtonElement[] {
    return menu === undefined ? [] : Array.from(menu.querySelectorAll<HTMLButtonElement>('[role="menuitemradio"]'));
  }

  // On opening, and whenever a new listing replaces the items while open:
  // keep the popover inside the window, and put the focus on the checked item
  // unless an item already has it.
  $effect(() => {
    if (!open) return;
    const values = ['', ...others];
    const target = checked;
    void tick().then(() => {
      if (open && popover !== undefined && trigger !== undefined) {
        const r = trigger.getBoundingClientRect();
        left = `${String(Math.max(16, Math.min(r.left, innerWidth - popover.offsetWidth - 16)))}px`;
        if (menu !== undefined && !menu.contains(document.activeElement)) items()[values.indexOf(target)]?.focus();
      }
    });
  });

  function onMenuKeydown(e: KeyboardEvent): void {
    const all = items();
    const at = all.findIndex((b) => b === document.activeElement);
    const go = (i: number): void => {
      e.preventDefault();
      all[(i + all.length) % all.length]?.focus();
    };
    switch (e.key) {
      case 'ArrowDown':
        go(at + 1);
        break;
      case 'ArrowUp':
        go(Math.max(at, 0) - 1);
        break;
      case 'Home':
        go(0);
        break;
      case 'End':
        go(all.length - 1);
        break;
      case 'Enter':
      case ' ':
        if (at >= 0) {
          e.preventDefault();
          all[at]?.click();
        }
        break;
      case 'Escape':
        e.preventDefault();
        closeAndRefocus();
        break;
      case 'Tab':
        close();
        break;
    }
  }

  function onWindowPointerdown(e: PointerEvent): void {
    if (!open) return;
    const target = e.target;
    if (target instanceof Node && (trigger?.contains(target) || popover?.contains(target))) return;
    close();
  }
</script>

<svelte:window onpointerdown={onWindowPointerdown} onresize={close} />

<div class="mic-row">
  <button
    type="button"
    class="mic quiet"
    aria-haspopup="menu"
    aria-expanded={open}
    aria-controls="microphones"
    aria-label={name === null ? 'Microphone' : `Microphone: ${name}`}
    title={name ?? undefined}
    bind:this={trigger}
    onpointerdown={() => {
      viaPointer = true;
    }}
    onclick={() => {
      if (open) close();
      else show();
    }}
    oncontextmenu={(e) => {
      e.preventDefault();
      viaPointer = true;
      show();
    }}
  >
    <svg aria-hidden="true" viewBox="0 0 16 16" width="16" height="16">
      <rect x="5.5" y="1.5" width="5" height="8.5" rx="2.5" fill="none" stroke="currentColor" />
      <path d="M3 7.5a5 5 0 0 0 10 0" fill="none" stroke="currentColor" />
      <line x1="8" y1="12.5" x2="8" y2="14.5" stroke="currentColor" />
    </svg>
    <span class="mic-name">{name ?? 'Microphone'}</span>
  </button>
</div>

{#if open}
  <div
    class="mic-popover"
    bind:this={popover}
    style:top
    style:bottom
    style:left
    style:max-height={maxHeight}
  >
    {#if error !== null}
      <p class="error" role="alert">{error}</p>
    {/if}
    <div id="microphones" role="menu" aria-label="Microphone" tabindex="-1" bind:this={menu} onkeydown={onMenuKeydown}>
      <button
        type="button"
        role="menuitemradio"
        aria-checked={checked === ''}
        tabindex="-1"
        title={defaultLabel}
        onclick={() => {
          choose('');
        }}
      >
        <span class="check" aria-hidden="true">{checked === '' ? '✓' : ''}</span>
        <span class="mic-item">{defaultLabel}</span>
      </button>
      {#each others as other (other)}
        <button
          type="button"
          role="menuitemradio"
          aria-checked={checked === other}
          tabindex="-1"
          title={other}
          onclick={() => {
            choose(other);
          }}
        >
          <span class="check" aria-hidden="true">{checked === other ? '✓' : ''}</span>
          <span class="mic-item">{other}</span>
        </button>
      {/each}
    </div>
  </div>
{/if}
