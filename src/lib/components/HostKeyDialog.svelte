<script lang="ts">
  // SSH host-key approval (trust-on-first-use). Surfaced when an SFTP connect
  // returns `hostKeyUnverified`. On a CHANGED key (a different key is already
  // pinned — possible MITM) we show a scary warning and do NOT offer "remember":
  // Wonderblob never silently overwrites a known host key in v1.
  let {
    host,
    port,
    fingerprint,
    changed,
    onaccept,
    oncancel,
  }: {
    host: string;
    port: number;
    fingerprint: string;
    changed: boolean;
    onaccept: (remember: boolean) => void;
    oncancel: () => void;
  } = $props();

  let panelEl = $state<HTMLDivElement | null>(null);
  let busy = $state(false);

  $effect(() => {
    panelEl?.focus();
  });

  function accept(remember: boolean) {
    if (busy) return;
    busy = true;
    onaccept(remember);
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      oncancel();
    }
  }
</script>

<div
  class="overlay"
  role="dialog"
  aria-modal="true"
  aria-label={changed ? "Host key changed" : "Unknown host key"}
  tabindex="-1"
  {onkeydown}
>
  <div class="backdrop" aria-hidden="true"></div>
  <div class="panel" class:danger={changed} bind:this={panelEl} tabindex="-1">
    {#if changed}
      <p class="title danger-text">⚠ HOST KEY CHANGED</p>
      <p class="body">
        The host key for <strong>{host}:{port}</strong> is different from the one
        Wonderblob previously trusted. This could mean someone is eavesdropping on
        you right now (a man-in-the-middle attack). It is also possible the server's
        key was just changed. Only continue if you know why the key changed.
      </p>
    {:else}
      <p class="title">Unknown host key</p>
      <p class="body">
        The server <strong>{host}:{port}</strong> presented a host key Wonderblob
        hasn't seen before. Verify the fingerprint below out-of-band before trusting it.
      </p>
    {/if}

    <div class="fp">
      <span class="fp-label">Fingerprint</span>
      <code class="selectable">{fingerprint}</code>
    </div>

    <div class="actions">
      {#if !changed}
        <button class="primary" disabled={busy} onclick={() => accept(true)}>
          Connect &amp; Remember
        </button>
      {/if}
      <button class="ghost" disabled={busy} onclick={() => accept(false)}>
        Connect Once
      </button>
      <button class="ghost" disabled={busy} onclick={() => oncancel()}>
        Cancel
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
    width: 420px;
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
  .panel.danger {
    border-color: var(--danger);
  }
  .title {
    margin: 0;
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg-primary);
  }
  .title.danger-text {
    color: var(--danger);
  }
  .body {
    margin: 0;
    font-size: var(--text-small);
    color: var(--fg-secondary);
    line-height: 1.45;
  }
  .fp {
    display: flex;
    flex-direction: column;
    gap: 3px;
    margin: 2px 0;
  }
  .fp-label {
    font-size: var(--text-small);
    color: var(--fg-secondary);
  }
  .selectable {
    user-select: text;
    font-family: var(--font-mono);
    font-size: var(--text-small);
    color: var(--fg-primary);
    word-break: break-all;
    padding: 6px 8px;
    background: var(--bg-hover);
    border-radius: var(--radius);
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
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.55;
    cursor: default;
  }
  .primary {
    color: #fff;
    background: var(--accent);
    border-color: var(--accent);
  }
  .primary:hover:not(:disabled) {
    filter: brightness(1.05);
  }
  .ghost {
    color: var(--fg-primary);
  }
  .ghost:hover:not(:disabled) {
    background: var(--bg-hover);
  }
</style>
