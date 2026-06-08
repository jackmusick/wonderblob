<script lang="ts">
  import { api, type Entry } from "../api";
  import { describeError, errorDetail } from "../errors";
  import { formatMtime, formatSize } from "../format";
  import { activeConnection, currentPath } from "../stores/session";

  let { onerror }: { onerror?: (message: string) => void } = $props();

  let entries = $state<Entry[]>([]);
  let selectedIndex = $state(-1);
  let loading = $state(false);
  let showSpinner = $state(false);
  let error = $state<{ message: string; detail: string } | null>(null);
  let confirmingDeletePath = $state<string | null>(null);
  let renamingPath = $state<string | null>(null);
  let renameValue = $state("");

  // FOCUS INVARIANT: keyboard focus always lives on containerEl (tabindex 0).
  // Rows use tabindex -1 so they never receive DOM focus. After any re-render
  // (descend, parent-nav, delete, rename, refresh) we call containerEl.focus()
  // if focus was already inside the list, keeping arrow/Enter/Backspace working
  // without requiring the user to click again.
  let containerEl: HTMLDivElement | null = null;
  let renameInput = $state<HTMLInputElement | null>(null);
  let seq = 0;
  let spinnerTimer: ReturnType<typeof setTimeout> | null = null;
  let confirmTimer: ReturnType<typeof setTimeout> | null = null;
  let typeahead = "";
  let typeaheadTimer: ReturnType<typeof setTimeout> | null = null;

  $effect(() => {
    const conn = $activeConnection;
    const path = $currentPath;
    if (!conn) return;
    load(conn.id, path);
  });

  $effect(() => {
    if (renamingPath && renameInput) {
      renameInput.focus();
      renameInput.select();
    }
  });

  /** True when focus is inside the list container (used to restore after re-render). */
  function listHasFocus(): boolean {
    return !!containerEl && containerEl.contains(document.activeElement);
  }

  /** Restore focus to the container if it was already there (keeps keyboard flow intact). */
  function restoreFocus() {
    if (listHasFocus() || document.activeElement === containerEl) {
      containerEl?.focus();
    }
  }

  async function load(id: number, path: string) {
    const mySeq = ++seq;
    loading = true;
    error = null;
    cancelRename();
    cancelConfirm();
    if (spinnerTimer) clearTimeout(spinnerTimer);
    spinnerTimer = setTimeout(() => {
      if (loading && mySeq === seq) showSpinner = true;
    }, 200);
    const hadFocus = listHasFocus();
    try {
      const result = await api.listDir(id, path);
      if (mySeq !== seq) return;
      entries = result;
      selectedIndex = result.length > 0 ? 0 : -1;
    } catch (e) {
      if (mySeq !== seq) return;
      entries = [];
      selectedIndex = -1;
      error = { message: describeError(e, "list"), detail: errorDetail(e) };
    } finally {
      if (mySeq === seq) {
        loading = false;
        showSpinner = false;
        if (spinnerTimer) clearTimeout(spinnerTimer);
        // Restore keyboard focus if it was inside the list before the re-render.
        if (hadFocus) requestAnimationFrame(() => containerEl?.focus());
      }
    }
  }

  export function refresh() {
    const conn = $activeConnection;
    if (conn) load(conn.id, $currentPath);
  }

  export function selected(): Entry | null {
    return selectedIndex >= 0 && selectedIndex < entries.length ? entries[selectedIndex] : null;
  }

  function cancelConfirm() {
    if (confirmTimer) clearTimeout(confirmTimer);
    confirmingDeletePath = null;
  }

  function cancelRename() {
    renamingPath = null;
    renameValue = "";
  }

  function parentOf(path: string): string {
    if (path === "/" || path === "") return "/";
    const trimmed = path.replace(/\/+$/, "");
    const idx = trimmed.lastIndexOf("/");
    return idx <= 0 ? "/" : trimmed.slice(0, idx);
  }

  function open(entry: Entry) {
    if (entry.kind === "dir") {
      currentPath.set(entry.path);
    } else {
      console.info("open: EditSession in Plan 4");
    }
  }

  function scrollSelectedIntoView() {
    requestAnimationFrame(() => {
      containerEl
        ?.querySelector('[aria-selected="true"]')
        ?.scrollIntoView({ block: "nearest" });
    });
  }

  function moveSelection(delta: number) {
    if (entries.length === 0) return;
    selectedIndex = Math.min(Math.max(selectedIndex + delta, 0), entries.length - 1);
    cancelConfirm();
    scrollSelectedIntoView();
  }

  function requestDelete(entry: Entry) {
    if (confirmingDeletePath === entry.path) {
      cancelConfirm();
      doDelete(entry);
    } else {
      if (confirmTimer) clearTimeout(confirmTimer);
      confirmingDeletePath = entry.path;
      confirmTimer = setTimeout(() => (confirmingDeletePath = null), 3000);
    }
  }

  async function doDelete(entry: Entry) {
    const conn = $activeConnection;
    if (!conn) return;
    try {
      await api.deleteEntry(conn.id, entry.path);
      refresh();
    } catch (e) {
      onerror?.(describeError(e, "mutate"));
    }
  }

  function startRename(entry: Entry) {
    renamingPath = entry.path;
    renameValue = entry.name;
  }

  async function commitRename(entry: Entry) {
    const conn = $activeConnection;
    const newName = renameValue.trim();
    cancelRename();
    containerEl?.focus();
    if (!conn || !newName || newName === entry.name) return;
    const to = `${parentOf(entry.path)}/${newName}`.replace(/^\/+/, "/");
    try {
      await api.renameEntry(conn.id, entry.path, to);
      refresh();
    } catch (e) {
      onerror?.(describeError(e, "mutate"));
    }
  }

  function handleTypeahead(char: string) {
    if (typeaheadTimer) clearTimeout(typeaheadTimer);
    typeahead += char.toLowerCase();
    typeaheadTimer = setTimeout(() => (typeahead = ""), 700);
    const match = entries.findIndex((e) => e.name.toLowerCase().startsWith(typeahead));
    if (match >= 0) {
      selectedIndex = match;
      scrollSelectedIntoView();
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (renamingPath) return;
    const selected = selectedIndex >= 0 ? entries[selectedIndex] : null;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      moveSelection(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      moveSelection(-1);
    } else if (e.key === "Enter" && selected) {
      e.preventDefault();
      open(selected);
    } else if (e.key === "Backspace") {
      e.preventDefault();
      const parent = parentOf($currentPath);
      if (parent !== $currentPath) currentPath.set(parent);
    } else if (e.key === "Delete" && selected) {
      e.preventDefault();
      requestDelete(selected);
    } else if (e.key === "F2" && selected && !e.ctrlKey && !e.metaKey && !e.altKey) {
      // F2 only – bare letter keys are reserved for type-ahead (matches desktop conventions).
      e.preventDefault();
      startRename(selected);
    } else if (e.key === "Escape") {
      cancelConfirm();
    } else if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      handleTypeahead(e.key);
    }
  }

  function renameKeydown(e: KeyboardEvent, entry: Entry) {
    if (e.key === "Enter") {
      e.preventDefault();
      commitRename(entry);
    } else if (e.key === "Escape") {
      e.preventDefault();
      cancelRename();
      containerEl?.focus();
    }
    e.stopPropagation();
  }
</script>

<div
  class="filelist"
  bind:this={containerEl}
  role="listbox"
  aria-label="Files"
  tabindex="0"
  onkeydown={onkeydown}
>
  <div class="header" role="presentation">
    <span class="col-name">Name</span>
    <span class="col-size">Size</span>
    <span class="col-mtime">Modified</span>
  </div>

  {#if showSpinner}
    <div class="state"><span class="spinner" aria-label="Loading"></span></div>
  {:else if error}
    <div class="state">
      <p class="error-message">{error.message}</p>
      {#if error.detail}
        <p class="error-detail" title={error.detail}>{error.detail}</p>
      {/if}
    </div>
  {:else if !loading && entries.length === 0}
    <div class="state"><p class="muted">Empty folder</p></div>
  {:else}
    {#each entries as entry, i (entry.path)}
      <div
        class="row"
        class:selected={selectedIndex === i}
        role="option"
        aria-selected={selectedIndex === i}
        tabindex="-1"
        onclick={() => {
          selectedIndex = i;
          cancelConfirm();
          // Keep DOM focus on the container so keyboard flow continues
          // without needing a second click (focus invariant).
          containerEl?.focus();
        }}
        ondblclick={() => { open(entry); containerEl?.focus(); }}
        onkeydown={() => {}}
      >
        <span class="col-name">
          <span class="glyph" class:dir={entry.kind === "dir"} aria-hidden="true">
            {entry.kind === "dir" ? "▸" : entry.kind === "symlink" ? "↪" : "▢"}
          </span>
          {#if renamingPath === entry.path}
            <input
              class="rename-input"
              bind:this={renameInput}
              bind:value={renameValue}
              onkeydown={(e) => renameKeydown(e, entry)}
              onblur={() => cancelRename()}
              aria-label="Rename {entry.name}"
            />
          {:else}
            <span class="name" title={entry.name}>{entry.name}</span>
          {/if}
          {#if confirmingDeletePath === entry.path}
            <span class="confirm">Delete? Press again to confirm</span>
          {/if}
        </span>
        <span class="col-size">{formatSize(entry.size)}</span>
        <span class="col-mtime">{formatMtime(entry.modifiedMs)}</span>
      </div>
    {/each}
  {/if}
</div>

<style>
  .filelist {
    display: flex;
    flex-direction: column;
    min-height: 100%;
    outline: none;
  }
  .filelist:focus .row.selected {
    outline: 1px solid var(--accent);
    outline-offset: -1px;
  }
  .header {
    position: sticky;
    top: 0;
    z-index: 1;
    display: flex;
    align-items: center;
    height: 26px;
    padding: 0 12px;
    background: var(--bg-content);
    border-bottom: 1px solid var(--border);
    font-size: var(--text-small);
    color: var(--fg-secondary);
    user-select: none;
  }
  .row {
    display: flex;
    align-items: center;
    height: var(--row-height);
    padding: 0 12px;
    font-size: var(--text-base);
    color: var(--fg-primary);
    user-select: none;
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.selected {
    background: var(--bg-selected);
  }
  .col-name {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .glyph {
    flex-shrink: 0;
    width: 14px;
    text-align: center;
    color: var(--fg-secondary);
    font-size: var(--text-small);
  }
  .glyph.dir {
    color: var(--accent);
  }
  .col-size {
    width: 90px;
    text-align: right;
    font-family: var(--font-mono);
    font-size: var(--text-small);
    color: var(--fg-secondary);
    flex-shrink: 0;
  }
  .col-mtime {
    width: 160px;
    text-align: right;
    font-size: var(--text-small);
    color: var(--fg-secondary);
    flex-shrink: 0;
  }
  .rename-input {
    flex: 1;
    min-width: 0;
    height: 22px;
    padding: 0 5px;
    font-size: var(--text-base);
    font-family: var(--font-ui);
    color: var(--fg-primary);
    background: var(--bg-content);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    outline: none;
  }
  .confirm {
    flex-shrink: 0;
    font-size: var(--text-small);
    color: var(--danger);
  }
  .state {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 4px;
    padding: 32px;
    text-align: center;
  }
  .muted {
    margin: 0;
    font-size: var(--text-base);
    color: var(--fg-secondary);
  }
  .error-message {
    margin: 0;
    font-size: var(--text-base);
    color: var(--fg-primary);
  }
  .error-detail {
    margin: 0;
    max-width: 420px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-small);
    color: var(--fg-secondary);
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
