<script lang="ts">
  import { api, type Entry, type PreviewResult } from "../api";
  import { describeError } from "../errors";
  import { formatSize } from "../format";

  let {
    entry,
    connectionId,
    onclose,
    onopen,
  }: {
    entry: Entry;
    connectionId: number;
    onclose: () => void;
    onopen: (e: Entry) => void;
  } = $props();

  let result = $state<PreviewResult | null>(null);
  let error = $state<string | null>(null);
  let host = $state<HTMLDivElement | null>(null);

  $effect(() => {
    host?.focus();
  });

  $effect(() => {
    let alive = true;
    api
      .previewFile(connectionId, entry.path, entry.name, entry.size ?? undefined)
      .then((r) => {
        if (alive) result = r;
      })
      .catch((e) => {
        if (alive) error = describeError(e, "mutate");
      });
    return () => {
      alive = false;
    };
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape" || e.key === " ") {
      e.preventDefault();
      e.stopPropagation();
      onclose();
    }
  }

  function openInEditor() {
    onopen(entry);
    onclose();
  }
</script>

<div class="overlay" bind:this={host} tabindex="0" role="dialog" aria-label="Preview" {onkeydown}>
  <div class="bar">
    <span class="name" title={entry.name}>{entry.name}</span>
    <button class="ghost" onclick={onclose} aria-label="Close preview">✕</button>
  </div>
  <div class="body">
    {#if error}
      <p class="msg danger">{error}</p>
    {:else if !result}
      <span class="spinner" aria-label="Loading"></span>
    {:else if result.plan.kind === "text"}
      <pre class="text selectable">{result.text}</pre>
    {:else if result.plan.kind === "image"}
      <img class="img" src={result.dataUrl} alt={entry.name} />
    {:else}
      <div class="fallback">
        <p class="msg">
          {#if result.plan.kind === "pdf"}PDF preview isn’t supported here.
          {:else if result.plan.kind === "tooLarge"}Too large to preview ({formatSize(
              entry.size,
            )}).
          {:else}Can’t preview .{result.plan.ext} files.{/if}
        </p>
        <button class="primary" onclick={openInEditor}>Open in editor</button>
      </div>
    {/if}
  </div>
</div>

<style>
  .overlay {
    position: absolute;
    inset: 0;
    z-index: 5;
    display: flex;
    flex-direction: column;
    background: var(--bg-content);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    outline: none;
    animation: fade-in 150ms ease-out;
  }
  @keyframes fade-in {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }
  .bar {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    height: 32px;
    padding: 0 8px 0 12px;
    border-bottom: 1px solid var(--border);
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    color: var(--fg-primary);
  }
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: auto;
    padding: 12px;
  }
  .text {
    margin: 0;
    align-self: stretch;
    flex: 1;
    overflow: auto;
    white-space: pre;
    font-family: var(--font-mono);
    font-size: var(--text-small);
    color: var(--fg-primary);
  }
  .selectable {
    user-select: text;
    -webkit-user-select: text;
  }
  .img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
  }
  .fallback {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    text-align: center;
  }
  .msg {
    margin: 0;
    font-size: var(--text-base);
    color: var(--fg-secondary);
  }
  .msg.danger {
    color: var(--danger);
  }
  .ghost {
    height: 22px;
    padding: 0 8px;
    font-size: var(--text-base);
    font-family: var(--font-ui);
    color: var(--fg-secondary);
    background: transparent;
    border: none;
    border-radius: var(--radius);
  }
  .ghost:hover {
    background: var(--bg-hover);
    color: var(--fg-primary);
  }
  .primary {
    height: 28px;
    padding: 0 14px;
    font-size: var(--text-base);
    font-family: var(--font-ui);
    color: #fff;
    background: var(--accent);
    border: none;
    border-radius: var(--radius);
  }
  .primary:hover {
    filter: brightness(1.08);
  }
  .spinner {
    width: 16px;
    height: 16px;
    border: 2px solid var(--border);
    border-top-color: var(--fg-secondary);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
