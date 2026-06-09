<script lang="ts" module>
  export type MenuItem =
    | { separator: true }
    | {
        label: string;
        icon?: string;
        action: () => void;
        danger?: boolean;
        disabled?: boolean;
      };
</script>

<script lang="ts">
  import Icon from "./Icon.svelte";

  let {
    x,
    y,
    items,
    onclose,
  }: { x: number; y: number; items: MenuItem[]; onclose: () => void } = $props();

  // Clamp the menu inside the viewport once it's measured (flip away from
  // edges). An action takes the coords as an argument, so there's no reactive
  // prop-capture to worry about — the menu is remounted per open anyway.
  function clamp(node: HTMLDivElement, coords: { x: number; y: number }) {
    const pad = 6;
    let c = coords;
    const apply = () => {
      const r = node.getBoundingClientRect();
      let left = c.x;
      let top = c.y;
      if (left + r.width + pad > window.innerWidth)
        left = Math.max(pad, window.innerWidth - r.width - pad);
      if (top + r.height + pad > window.innerHeight)
        top = Math.max(pad, window.innerHeight - r.height - pad);
      node.style.left = `${left}px`;
      node.style.top = `${top}px`;
    };
    apply();
    return {
      update(next: { x: number; y: number }) {
        c = next;
        apply();
      },
    };
  }

  function choose(item: Extract<MenuItem, { action: () => void }>) {
    if (item.disabled) return;
    onclose();
    item.action();
  }

  function onkey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onclose();
    }
  }
</script>

<svelte:window onkeydown={onkey} onresize={onclose} />

<!-- Full-screen catcher: any outside press (or a fresh right-click) dismisses. -->
<div
  class="backdrop"
  onpointerdown={onclose}
  oncontextmenu={(e) => {
    e.preventDefault();
    onclose();
  }}
  aria-hidden="true"
></div>

<div
  class="menu"
  use:clamp={{ x, y }}
  style="left:{x}px; top:{y}px"
  role="menu"
  tabindex="-1"
>
  {#each items as item}
    {#if "separator" in item}
      <div class="sep" role="separator"></div>
    {:else}
      <button
        class="item"
        class:danger={item.danger}
        disabled={item.disabled}
        role="menuitem"
        onclick={() => choose(item)}
      >
        {#if item.icon}<Icon name={item.icon} size={15} />{:else}<span
            class="ico-spacer"
          ></span>{/if}
        <span class="label">{item.label}</span>
      </button>
    {/if}
  {/each}
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1000;
  }
  .menu {
    position: fixed;
    z-index: 1001;
    min-width: 184px;
    padding: 5px;
    /* Elevated surface — a notch above the content plane, with a brighter
       hairline and a deeper shadow so the menu reads as floating. */
    background: var(--bg-elevated);
    border: 1px solid var(--border-strong);
    border-radius: 8px;
    box-shadow:
      0 12px 34px rgba(0, 0, 0, 0.45),
      0 1px 0 rgba(255, 255, 255, 0.04) inset;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 9px;
    height: 28px;
    padding: 0 8px;
    background: transparent;
    border: none;
    border-radius: 5px;
    color: var(--fg-primary);
    font-family: var(--font-ui);
    font-size: var(--text-base);
    font-weight: var(--weight-label);
    text-align: left;
    cursor: default;
  }
  /* Accent-tinted hover (1Password-style) instead of a flat gray wash. */
  .item:hover:not(:disabled) {
    background: var(--bg-selected);
    color: var(--fg-primary);
  }
  .item:disabled {
    opacity: 0.4;
  }
  .item.danger {
    color: var(--danger);
  }
  .item.danger:hover:not(:disabled) {
    background: color-mix(in srgb, var(--danger) 18%, transparent);
  }
  .label {
    flex: 1;
  }
  .ico-spacer {
    width: 15px;
    height: 15px;
    flex: none;
  }
  /* Full-bleed divider with a brighter line than the body hairlines. */
  .sep {
    height: 1px;
    margin: 4px -5px;
    background: var(--border-strong);
  }
</style>
