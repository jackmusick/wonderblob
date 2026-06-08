<script lang="ts">
  import { api, type Bookmark, type StorageError } from "../api";
  import { activeConnection, currentPath } from "../stores/session";

  let {
    onnew,
    onedit,
  }: {
    onnew: () => void;
    onedit: (b: Bookmark) => void;
  } = $props();

  let bookmarks = $state<Bookmark[]>([]);
  let focusedIndex = $state(-1);
  let connectingId = $state<string | null>(null);
  let errors = $state<Record<string, { message: string; detail: string }>>({});
  let confirmingDeleteId = $state<string | null>(null);
  let confirmTimer: ReturnType<typeof setTimeout> | null = null;

  export async function reload() {
    bookmarks = await api.bookmarksList();
  }

  $effect(() => {
    reload();
  });

  function errorMessage(e: unknown): { message: string; detail: string } {
    const err = e as StorageError;
    const detail = typeof err?.detail === "string" ? err.detail : String(e);
    switch (err?.kind) {
      case "authFailed":
        return { message: "Authentication failed", detail };
      case "network":
        return { message: "Can't reach server", detail };
      default:
        return { message: "Connection failed", detail };
    }
  }

  async function connect(b: Bookmark) {
    if (connectingId) return;
    connectingId = b.id;
    const { [b.id]: _, ...rest } = errors;
    errors = rest;
    try {
      const id = await api.connectBookmark(b.id);
      activeConnection.set({ id, bookmark: b });
      currentPath.set(b.initialPath ?? "/");
    } catch (e) {
      errors = { ...errors, [b.id]: errorMessage(e) };
    } finally {
      connectingId = null;
    }
  }

  function requestDelete(b: Bookmark) {
    if (confirmingDeleteId === b.id) {
      if (confirmTimer) clearTimeout(confirmTimer);
      confirmingDeleteId = null;
      doDelete(b);
    } else {
      confirmingDeleteId = b.id;
      if (confirmTimer) clearTimeout(confirmTimer);
      confirmTimer = setTimeout(() => (confirmingDeleteId = null), 3000);
    }
  }

  async function doDelete(b: Bookmark) {
    try {
      await api.bookmarkDelete(b.id);
      const active = $activeConnection;
      if (active?.bookmark.id === b.id) {
        api.disconnect(active.id).catch(() => {});
        activeConnection.set(null);
      }
      await reload();
      if (focusedIndex >= bookmarks.length) focusedIndex = bookmarks.length - 1;
    } catch (e) {
      errors = { ...errors, [b.id]: errorMessage(e) };
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (bookmarks.length === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      focusedIndex = Math.min(focusedIndex + 1, bookmarks.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      focusedIndex = Math.max(focusedIndex - 1, 0);
    } else if (e.key === "Enter" && focusedIndex >= 0) {
      e.preventDefault();
      connect(bookmarks[focusedIndex]);
    } else if (e.key === "Delete" && focusedIndex >= 0) {
      e.preventDefault();
      requestDelete(bookmarks[focusedIndex]);
    } else if (e.key === "F2" && focusedIndex >= 0) {
      // F2 only – matches desktop conventions; bare 'e' is not bound here.
      e.preventDefault();
      onedit(bookmarks[focusedIndex]);
    }
  }
</script>

<div class="section-header">
  <span class="section-label">Connections</span>
  <button class="icon-btn" title="New connection" aria-label="New connection" onclick={onnew}>
    +
  </button>
</div>

<div
  class="list"
  role="listbox"
  aria-label="Saved connections"
  tabindex="0"
  onkeydown={onkeydown}
>
  {#each bookmarks as b, i (b.id)}
    {@const selected = $activeConnection?.bookmark.id === b.id}
    <div class="row-wrap">
      <div
        class="row"
        class:selected
        class:focused={focusedIndex === i}
        role="option"
        aria-selected={selected}
        tabindex="-1"
        ondblclick={() => connect(b)}
        onclick={() => (focusedIndex = i)}
        onkeydown={() => {}}
      >
        <span class="label" title="{b.username}@{b.host}:{b.port}">{b.label}</span>
        {#if connectingId === b.id}
          <span class="hint">connecting…</span>
        {:else}
          <span class="row-actions">
            <button
              class="icon-btn"
              title="Edit"
              aria-label="Edit {b.label}"
              onclick={(e) => {
                e.stopPropagation();
                onedit(b);
              }}>✎</button
            >
            <button
              class="icon-btn"
              class:confirming={confirmingDeleteId === b.id}
              title="Delete"
              aria-label="Delete {b.label}"
              onclick={(e) => {
                e.stopPropagation();
                requestDelete(b);
              }}>{confirmingDeleteId === b.id ? "Delete?" : "×"}</button
            >
          </span>
        {/if}
      </div>
      {#if errors[b.id]}
        <div class="error" title={errors[b.id].detail}>{errors[b.id].message}</div>
      {/if}
    </div>
  {/each}
</div>

<style>
  .section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 8px;
  }
  .section-label {
    font-size: var(--text-small);
    font-weight: 600;
    color: var(--fg-secondary);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .list {
    outline: none;
    border-radius: var(--radius);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    height: var(--row-height);
    padding: 0 8px;
    border-radius: var(--radius);
    cursor: default;
    user-select: none;
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.selected {
    background: var(--bg-selected);
  }
  .list:focus .row.focused {
    outline: 1px solid var(--accent);
    outline-offset: -1px;
  }
  .label {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    color: var(--fg-primary);
  }
  .hint {
    font-size: var(--text-small);
    color: var(--fg-secondary);
  }
  /* Hidden until hover or keyboard focus lands inside the row, but the
     buttons stay rendered so they remain in the tab order. */
  .row-actions {
    display: flex;
    gap: 2px;
    opacity: 0;
  }
  .row:hover .row-actions,
  .row:focus-within .row-actions {
    opacity: 1;
  }
  .icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 20px;
    height: 20px;
    padding: 0 3px;
    font-size: var(--text-base);
    font-family: var(--font-ui);
    color: var(--fg-secondary);
    background: transparent;
    border: none;
    border-radius: var(--radius);
  }
  .icon-btn:hover {
    background: var(--bg-hover);
    color: var(--fg-primary);
  }
  .icon-btn.confirming {
    color: var(--fg-primary);
    background: var(--bg-hover);
    font-size: var(--text-small);
    font-weight: 600;
  }
  .error {
    font-size: var(--text-small);
    color: var(--danger);
    padding: 1px 8px 4px;
  }
</style>
