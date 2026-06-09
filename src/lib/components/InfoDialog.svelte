<script lang="ts">
  import type { Entry } from "$lib/api";
  import { formatMtime, formatSize } from "$lib/format";

  // Read-only file/folder info. The parent gates rendering on a non-null entry.
  let { entry, onclose }: { entry: Entry; onclose: () => void } = $props();

  let panelEl = $state<HTMLDivElement | null>(null);

  $effect(() => {
    panelEl?.focus();
  });

  const kindLabel = (k: Entry["kind"]) =>
    k === "dir" ? "Folder" : k === "symlink" ? "Symlink" : "File";

  // Exact byte count is worth showing alongside the human size for files.
  const exactBytes = (n: number | null) =>
    n === null ? null : `${n.toLocaleString()} bytes`;

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onclose();
    }
  }
</script>

<div
  class="overlay"
  role="dialog"
  aria-modal="true"
  aria-label="File info"
  tabindex="-1"
  {onkeydown}
>
  <div class="backdrop" aria-hidden="true" onclick={onclose}></div>
  <div class="panel" bind:this={panelEl} tabindex="-1">
    <p class="title" title={entry.name}>{entry.name}</p>
    <dl class="rows">
      <dt>Kind</dt>
      <dd>{kindLabel(entry.kind)}</dd>
      <dt>Path</dt>
      <dd class="mono path" title={entry.path}>{entry.path}</dd>
      {#if entry.kind !== "dir"}
        <dt>Size</dt>
        <dd>
          {formatSize(entry.size)}{#if exactBytes(entry.size)}
            <span class="muted"> ({exactBytes(entry.size)})</span>{/if}
        </dd>
      {/if}
      <dt>Modified</dt>
      <dd>{formatMtime(entry.modifiedMs)}</dd>
    </dl>
    <div class="actions">
      <button class="ghost" onclick={onclose}>Close</button>
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
    gap: 12px;
    outline: none;
  }
  .title {
    margin: 0;
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rows {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 6px 14px;
    margin: 0;
    font-size: var(--text-small);
  }
  dt {
    color: var(--fg-secondary);
  }
  dd {
    margin: 0;
    color: var(--fg-primary);
    min-width: 0;
    word-break: break-word;
  }
  .mono {
    font-family: var(--font-mono);
  }
  .path {
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--fg-secondary);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
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
  .ghost {
    color: var(--fg-primary);
  }
  .ghost:hover {
    background: var(--bg-hover);
  }
</style>
