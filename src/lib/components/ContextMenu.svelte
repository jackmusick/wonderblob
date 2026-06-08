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
    padding: 4px;
    background: var(--bg-content);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 8px 28px rgba(0, 0, 0, 0.28);
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
    border-radius: 4px;
    color: var(--fg-primary);
    font-family: var(--font-ui);
    font-size: var(--text-base);
    text-align: left;
    cursor: default;
  }
  .item:hover:not(:disabled) {
    background: var(--bg-hover);
  }
  .item:disabled {
    opacity: 0.4;
  }
  .item.danger {
    color: var(--danger);
  }
  .label {
    flex: 1;
  }
  .ico-spacer {
    width: 15px;
    height: 15px;
    flex: none;
  }
  .sep {
    height: 1px;
    margin: 3px 6px;
    background: var(--border);
  }
</style>
