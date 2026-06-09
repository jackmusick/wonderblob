<script lang="ts">
  import type { EditSessionInfo } from "$lib/api";
  import { resolve } from "$lib/stores/edit";

  // The first pending conflict; the parent gates rendering on $editConflicts.length.
  let { session }: { session: EditSessionInfo } = $props();

  let busy = $state(false);
  let panelEl = $state<HTMLDivElement | null>(null);

  $effect(() => {
    panelEl?.focus();
  });

  async function act(action: "overwrite" | "saveAsCopy" | "discard") {
    if (busy) return;
    busy = true;
    try {
      await resolve(session.sessionId, action);
    } finally {
      busy = false;
    }
  }

  function onkeydown(e: KeyboardEvent) {
    // Esc = dismiss (leave unresolved); the badge persists until the user chooses.
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
    }
  }
</script>

<div class="overlay" role="dialog" aria-modal="true" aria-label="Resolve conflict" tabindex="-1" {onkeydown}>
  <div class="backdrop" aria-hidden="true"></div>
  <div class="panel" bind:this={panelEl} tabindex="-1">
    <p class="title">“{session.name}” changed on the server</p>
    <p class="body">
      This file changed on the server since you opened it. How do you want to resolve it?
    </p>
    <div class="actions">
      <button class="danger" disabled={busy} onclick={() => act("overwrite")}>
        Overwrite remote
      </button>
      <button class="ghost" disabled={busy} onclick={() => act("saveAsCopy")}>
        Save as copy
      </button>
      <button class="danger" disabled={busy} onclick={() => act("discard")}>
        Discard local changes
      </button>
    </div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 200;
  }
  .backdrop {
    position: absolute;
    inset: 0;
    background: rgba(0, 0, 0, 0.3);
  }
  .panel {
    position: relative;
    width: 400px;
    max-width: 90vw;
    background: var(--bg-elevated);
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    box-shadow: 0 16px 44px rgba(0, 0, 0, 0.45);
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    outline: none;
  }
  .title {
    margin: 0;
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg-primary);
  }
  .body {
    margin: 0;
    font-size: var(--text-small);
    color: var(--fg-secondary);
  }
  .actions {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 4px;
  }
  button {
    height: 28px;
    padding: 0 14px;
    font-size: var(--text-base);
    font-family: var(--font-ui);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: transparent;
  }
  button:disabled {
    opacity: 0.55;
  }
  .ghost {
    color: var(--fg-primary);
  }
  .ghost:hover:not(:disabled) {
    background: var(--bg-hover);
  }
  .danger {
    color: var(--danger);
    border-color: var(--danger);
  }
  .danger:hover:not(:disabled) {
    background: var(--bg-hover);
  }
</style>
