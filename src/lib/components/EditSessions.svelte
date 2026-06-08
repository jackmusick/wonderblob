<script lang="ts">
  import { closeSession, editSessions } from "$lib/stores/edit";

  let open = $state(false);
  let panelEl = $state<HTMLDivElement | null>(null);

  function toggle() {
    open = !open;
  }

  function onPanelKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      open = false;
    }
  }
</script>

{#if $editSessions.length > 0}
  <div class="wrap">
    <button class="ghost" onclick={toggle} aria-expanded={open}>
      Editing ({$editSessions.length})
    </button>
    {#if open}
      <div
        class="popover"
        bind:this={panelEl}
        role="menu"
        tabindex="-1"
        aria-label="Open for editing"
        onkeydown={onPanelKeydown}
      >
        {#each $editSessions as s (s.sessionId)}
          <div class="item">
            <div class="head">
              {#if s.hasConflict}
                <span class="dot" title="Conflict" aria-label="Conflict"></span>
              {/if}
              <span class="name" title={s.remotePath}>{s.name}</span>
            </div>
            <div class="actions">
              <button class="ghost" onclick={() => closeSession(s.sessionId, true)}>
                Close (keep file)
              </button>
              <button class="ghost" onclick={() => closeSession(s.sessionId, false)}>
                Close &amp; discard
              </button>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .wrap {
    position: relative;
    display: inline-flex;
  }
  .popover {
    position: absolute;
    top: 28px;
    right: 0;
    z-index: 50;
    width: 280px;
    max-height: 320px;
    overflow-y: auto;
    background: var(--bg-content);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.25);
    padding: 4px;
  }
  .item {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 6px 8px;
    border-radius: var(--radius);
  }
  .item:hover {
    background: var(--bg-hover);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .dot {
    flex-shrink: 0;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
  }
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    color: var(--fg-primary);
  }
  .actions {
    display: flex;
    gap: 4px;
  }
  .ghost {
    height: 22px;
    padding: 0 8px;
    font-size: var(--text-small);
    font-family: var(--font-ui);
    color: var(--fg-secondary);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    white-space: nowrap;
  }
  .ghost:hover {
    background: var(--bg-hover);
    color: var(--fg-primary);
  }
  .wrap > .ghost {
    border: none;
    height: 24px;
    padding: 0 9px;
    font-size: var(--text-base);
  }
</style>
