<script lang="ts">
  import { api, type Transfer } from "$lib/api";
  import { describeError } from "$lib/errors";
  import { formatSize } from "$lib/format";
  import { formatSpeed, percent } from "$lib/transfer-format";
  import { activeConnection } from "$lib/stores/session";
  import { clearCompleted, clearTransfer, transferList, transferSpeed } from "$lib/stores/transfers";
  import Icon from "./Icon.svelte";

  let {
    onerror,
    onclose,
  }: { onerror?: (message: string) => void; onclose?: () => void } = $props();

  // Roving focus across rows; action buttons remain Tab-reachable within a row.
  let rowEls = $state<HTMLElement[]>([]);

  function focusRow(i: number) {
    rowEls[i]?.focus();
  }

  function onRowKeydown(e: KeyboardEvent, i: number) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      focusRow(Math.min(i + 1, $transferList.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      focusRow(Math.max(i - 1, 0));
    } else if (e.key === "Escape") {
      e.preventDefault();
      onclose?.();
    }
  }

  async function run(fn: () => Promise<unknown>) {
    try {
      await fn();
    } catch (e) {
      onerror?.(describeError(e, "mutate"));
    }
  }

  const pause = (id: number) => run(() => api.pauseTransfer(id));
  // Resume rebinds restart-loaded transfers to the live connection.
  const resume = (id: number) => run(() => api.resumeTransfer(id, $activeConnection?.id));
  const cancel = (id: number) => run(() => api.cancelTransfer(id));

  function statusLabel(t: Transfer): string {
    switch (t.status) {
      case "queued":
        return "Queued";
      case "completed":
        return "Done";
      case "canceled":
        return "Canceled";
      case "failed":
        return "Failed";
      default:
        return "";
    }
  }
</script>

<div class="panel">
  {#if $transferList.length === 0}
    <div class="empty">No transfers yet</div>
  {:else}
    <div class="rows" role="listbox" aria-label="Transfers">
      {#each $transferList as t, i (t.id)}
        {@const pct = percent(t.transferredBytes, t.totalBytes)}
        <div
          class="row"
          role="option"
          aria-label={t.name}
          aria-selected="false"
          tabindex="0"
          bind:this={rowEls[i]}
          onkeydown={(e) => onRowKeydown(e, i)}
        >
          <span class="dir" aria-hidden="true" title={t.direction === "down" ? "Download" : "Upload"}>
            <Icon name={t.direction === "down" ? "download" : "upload"} size={13} />
          </span>
          <span class="name" title={t.name}>{t.name}</span>
          <div class="bar" class:indeterminate={pct === -1} aria-hidden="true">
            {#if pct >= 0}
              <div class="fill" style="width: {pct}%"></div>
            {/if}
          </div>
          <span class="meta">
            {#if pct >= 0}<span class="pct">{pct}%</span>{/if}
            <span class="bytes"
              >{formatSize(t.transferredBytes)}{#if t.totalBytes != null} / {formatSize(
                  t.totalBytes
                )}{/if}</span
            >
          </span>
          <span class="speed">{formatSpeed($transferSpeed.get(t.id) ?? 0)}</span>
          <span class="state">{statusLabel(t)}</span>
          <span class="actions">
            {#if t.status === "running"}
              <button class="ghost" onclick={() => pause(t.id)}>Pause</button>
              <button class="ghost" onclick={() => cancel(t.id)}>Cancel</button>
            {:else if t.status === "paused"}
              <button class="ghost" onclick={() => resume(t.id)}>Resume</button>
              <button class="ghost" onclick={() => cancel(t.id)}>Cancel</button>
            {:else if t.status === "queued"}
              <button class="ghost" onclick={() => cancel(t.id)}>Cancel</button>
            {:else if t.status === "failed"}
              <button class="ghost" onclick={() => resume(t.id)}>Retry</button>
            {/if}
            {#if t.status === "completed" || t.status === "canceled" || t.status === "failed"}
              <button
                class="icon-dismiss"
                title="Remove from list"
                aria-label="Remove {t.name} from list"
                onclick={() => run(() => clearTransfer(t.id))}
              >
                <Icon name="x" size={14} />
              </button>
            {/if}
          </span>
        </div>
        {#if t.status === "failed" && t.error}
          <div class="row-error" role="alert" title={t.error}>{t.error}</div>
        {/if}
      {/each}
    </div>
  {/if}
  <div class="footer">
    <button class="ghost" onclick={() => run(clearCompleted)}>Clear finished</button>
  </div>
</div>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    /* Fill the flex parent horizontally — without flex:1 the panel shrinks to
       content width, leaving a gap on the right that doesn't track resize. */
    flex: 1;
    min-width: 0;
    height: 100%;
    min-height: 0;
  }
  .rows {
    flex: 1;
    overflow-y: auto;
    min-height: 0;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--row-height);
    padding: 0 12px;
    font-size: var(--text-base);
    color: var(--fg-primary);
    outline: none;
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row:focus {
    outline: 1px solid var(--accent);
    outline-offset: -1px;
    background: var(--bg-selected);
  }
  .dir {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 14px;
    color: var(--fg-secondary);
  }
  /* Name hugs its content (truncating long names); the bar then fills the gap.
     Two flex-growing columns is what made the layout look "equally spaced". */
  .name {
    flex: 0 1 auto;
    min-width: 0;
    max-width: 42%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .bar {
    flex: 1 1 0;
    min-width: 60px;
    height: 5px;
    border-radius: var(--radius);
    background: var(--bg-hover);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    border-radius: var(--radius);
    transition: width 120ms linear;
  }
  /* Indeterminate (unknown total): a subtle moving accent sweep. */
  .bar.indeterminate {
    background-image: linear-gradient(
      90deg,
      transparent 0%,
      var(--accent) 50%,
      transparent 100%
    );
    background-size: 40% 100%;
    background-repeat: no-repeat;
    opacity: 0.5;
    animation: sweep 1.1s linear infinite;
  }
  @keyframes sweep {
    from {
      background-position: -40% 0;
    }
    to {
      background-position: 140% 0;
    }
  }
  .meta {
    flex-shrink: 0;
    display: flex;
    gap: 6px;
    width: 150px;
    justify-content: flex-end;
    font-family: var(--font-mono);
    font-size: var(--text-small);
    color: var(--fg-secondary);
  }
  .pct {
    color: var(--fg-primary);
  }
  .speed {
    flex-shrink: 0;
    width: 78px;
    text-align: right;
    font-family: var(--font-mono);
    font-size: var(--text-small);
    color: var(--fg-secondary);
  }
  .state {
    flex-shrink: 0;
    width: 64px;
    text-align: right;
    font-size: var(--text-small);
    color: var(--fg-secondary);
  }
  .actions {
    flex-shrink: 0;
    display: flex;
    gap: 2px;
    justify-content: flex-end;
    width: 130px;
  }
  .row-error {
    padding: 2px 12px 4px 32px;
    font-size: var(--text-small);
    color: var(--danger);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ghost {
    height: 22px;
    padding: 0 8px;
    font-size: var(--text-small);
    font-family: var(--font-ui);
    color: var(--fg-secondary);
    background: transparent;
    border: none;
    border-radius: var(--radius);
    white-space: nowrap;
  }
  .ghost:hover {
    background: var(--bg-hover);
    color: var(--fg-primary);
  }
  .icon-dismiss {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    background: transparent;
    border: none;
    border-radius: var(--radius);
    color: var(--fg-secondary);
  }
  .icon-dismiss:hover {
    background: var(--bg-hover);
    color: var(--fg-primary);
  }
  .empty {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: var(--text-base);
    color: var(--fg-secondary);
  }
  .footer {
    flex-shrink: 0;
    display: flex;
    justify-content: flex-end;
    padding: 4px 12px;
    border-top: 1px solid var(--border);
  }
</style>
