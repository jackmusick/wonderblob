<script lang="ts">
  import { SvelteMap, SvelteSet } from "svelte/reactivity";
  import { api, type Entry } from "../api";
  import { describeError, errorDetail } from "../errors";
  import { formatMtime, formatSize } from "../format";
  import { activeConnection, currentPath } from "../stores/session";
  import { editPaths } from "../stores/edit";
  import { prefs } from "../stores/prefs";
  import Icon from "./Icon.svelte";
  import ContextMenu, { type MenuItem } from "./ContextMenu.svelte";

  let {
    onerror,
    onpreview,
    // Parent-owned actions surfaced in the right-click menu. They act on the
    // current selection, and a right-click selects the row first, so no entry
    // argument is needed. Download = straight to ~/Downloads; DownloadAs = save
    // dialog (mirrors the toolbar).
    onDownload,
    onDownloadAs,
    onShare,
    onNewFolder,
    onUpload,
  }: {
    onerror?: (message: string) => void;
    onpreview?: (entry: Entry) => void;
    onDownload?: () => void;
    onDownloadAs?: () => void;
    onShare?: () => void;
    onNewFolder?: () => void;
    onUpload?: () => void;
  } = $props();

  // Right-click menu (row actions, or empty-area New Folder / Upload).
  let menu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null);

  const IMAGE_EXT = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico", "heic", "avif", "tiff"]);
  const CODE_EXT = new Set(["js", "ts", "jsx", "tsx", "rs", "py", "go", "rb", "java", "kt", "c", "cc", "cpp", "h", "hpp", "cs", "php", "swift", "sh", "bash", "zsh", "html", "css", "scss", "json", "yaml", "yml", "toml", "xml", "sql", "lua", "vue", "svelte"]);
  const ARCHIVE_EXT = new Set(["zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "zst", "lz", "lzma"]);
  const TEXT_EXT = new Set(["txt", "md", "markdown", "rtf", "log", "csv", "tsv", "ini", "cfg", "conf", "env"]);

  function iconForEntry(e: Entry): string {
    if (e.kind === "dir") return "folder";
    const ext = e.name.includes(".") ? e.name.toLowerCase().split(".").pop()! : "";
    if (IMAGE_EXT.has(ext)) return "file-image";
    if (CODE_EXT.has(ext)) return "file-code";
    if (ARCHIVE_EXT.has(ext)) return "file-archive";
    if (ext === "pdf") return "file-pdf";
    if (TEXT_EXT.has(ext)) return "file-text";
    return "file";
  }

  function rowMenuItems(entry: Entry): MenuItem[] {
    const canPresign = $activeConnection?.capabilities.canPresign ?? false;
    const items: MenuItem[] = [
      { label: "Open", icon: entry.kind === "dir" ? "folder" : "pencil", action: () => open(entry) },
    ];
    if (entry.kind !== "dir") {
      items.push(
        { label: "Download", icon: "download", action: () => onDownload?.() },
        { label: "Download As…", icon: "download", action: () => onDownloadAs?.() },
      );
      if (canPresign) items.push({ label: "Share Link", icon: "share", action: () => onShare?.() });
    }
    items.push(
      { separator: true },
      { label: "Rename", icon: "pencil", action: () => startRename(entry) },
      { label: "Delete", icon: "trash", danger: true, action: () => doDelete(entry) },
    );
    return items;
  }

  function openRowMenu(e: MouseEvent, i: number, entry: Entry) {
    e.preventDefault();
    e.stopPropagation();
    selectedIndex = i;
    cancelConfirm();
    containerEl?.focus();
    menu = { x: e.clientX, y: e.clientY, items: rowMenuItems(entry) };
  }

  function openEmptyMenu(e: MouseEvent) {
    e.preventDefault();
    menu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: "New Folder", icon: "folder-plus", action: () => onNewFolder?.() },
        { label: "Upload…", icon: "upload", action: () => onUpload?.() },
      ],
    };
  }

  // Right-click the column header → toggle which optional columns are shown.
  function toggleCol(key: "size" | "modified") {
    prefs.update((p) => ({ ...p, columns: { ...p.columns, [key]: !p.columns[key] } }));
  }
  function openHeaderMenu(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    menu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: "Size", icon: $prefs.columns.size ? "check" : undefined, action: () => toggleCol("size") },
        { label: "Modified", icon: $prefs.columns.modified ? "check" : undefined, action: () => toggleCol("modified") },
      ],
    };
  }

  // Drag a column's left divider to resize it; the flexible Name column absorbs
  // the slack. Persisted via the prefs store.
  function startColResize(e: PointerEvent, key: "size" | "modified") {
    e.preventDefault();
    e.stopPropagation();
    const startX = e.clientX;
    const startW = $prefs.colWidths[key];
    const onMove = (ev: PointerEvent) => {
      const w = Math.min(400, Math.max(50, startW - (ev.clientX - startX)));
      prefs.update((p) => ({ ...p, colWidths: { ...p.colWidths, [key]: w } }));
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  }

  // Transient "Opening…" hint while an EditSession download is in flight.
  let opening = $state(false);

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

  // Sorting. `entries` is kept in sorted order (dirs always before files), so
  // all the index-based selection logic below is unaffected.
  let sortCol = $state<"name" | "size" | "mtime">("name");
  let sortDir = $state<1 | -1>(1);

  function cmp(a: Entry, b: Entry): number {
    const ad = a.kind === "dir" ? 0 : 1;
    const bd = b.kind === "dir" ? 0 : 1;
    if (ad !== bd) return ad - bd; // dirs first, regardless of direction
    let r: number;
    if (sortCol === "size") r = (a.size ?? 0) - (b.size ?? 0);
    else if (sortCol === "mtime") r = (a.modifiedMs ?? 0) - (b.modifiedMs ?? 0);
    else r = a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });
    if (r === 0)
      r = a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });
    return r * sortDir;
  }

  function sortEntries(arr: Entry[]): Entry[] {
    return [...arr].sort(cmp);
  }

  // Tree view: directories expand inline, lazily loading their children. The
  // flat `entries` is the top level; `childrenMap` holds each expanded dir's
  // listing. `visibleRows` flattens that tree to the rows actually shown, so the
  // index-based selection/keyboard logic keeps working over a single array.
  const expanded = new SvelteSet<string>();
  const childrenMap = new SvelteMap<string, Entry[]>();
  const loadingChildren = new SvelteSet<string>();

  const visibleRows = $derived.by(() => {
    const out: { entry: Entry; depth: number }[] = [];
    const walk = (list: Entry[], depth: number) => {
      for (const e of list) {
        out.push({ entry: e, depth });
        if (e.kind === "dir" && expanded.has(e.path)) {
          const kids = childrenMap.get(e.path);
          if (kids) walk(kids, depth + 1);
        }
      }
    };
    walk(entries, 0);
    return out;
  });

  function reanchor(path: string | null) {
    if (!path) return;
    const idx = visibleRows.findIndex((r) => r.entry.path === path);
    if (idx >= 0) selectedIndex = idx;
  }

  async function toggleExpand(entry: Entry) {
    if (entry.kind !== "dir") return;
    const path = entry.path;
    const keep = selected()?.path ?? null;
    if (expanded.has(path)) {
      expanded.delete(path);
      reanchor(keep);
      return;
    }
    expanded.add(path);
    if (!childrenMap.has(path)) {
      const conn = $activeConnection;
      if (conn) {
        loadingChildren.add(path);
        try {
          childrenMap.set(path, sortEntries(await api.listDir(conn.id, path)));
        } catch (e) {
          expanded.delete(path);
          onerror?.(describeError(e, "list"));
        } finally {
          loadingChildren.delete(path);
        }
      }
    }
    reanchor(keep);
  }

  function setSort(col: "name" | "size" | "mtime") {
    const keep = selected()?.path ?? null;
    if (sortCol === col) sortDir = sortDir === 1 ? -1 : 1;
    else {
      sortCol = col;
      sortDir = 1;
    }
    entries = sortEntries(entries);
    for (const [k, v] of childrenMap) childrenMap.set(k, sortEntries(v));
    reanchor(keep);
  }

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
      // New directory = fresh tree; drop any prior expansion state.
      expanded.clear();
      childrenMap.clear();
      loadingChildren.clear();
      entries = sortEntries(result);
      selectedIndex = entries.length > 0 ? 0 : -1;
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
    return visibleRows[selectedIndex]?.entry ?? null;
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

  async function open(entry: Entry) {
    if (entry.kind === "dir") {
      currentPath.set(entry.path);
      return;
    }
    const conn = $activeConnection;
    if (!conn) return;
    opening = true;
    try {
      await api.openInEditor(conn.id, entry.path); // download → OS default app → watch
    } catch (e) {
      onerror?.(describeError(e, "mutate"));
    } finally {
      opening = false;
    }
  }

  /** Open-in-editor entry point reused by the preview overlay's fallback button. */
  export function openEntry(entry: Entry) {
    open(entry);
  }

  function scrollSelectedIntoView() {
    requestAnimationFrame(() => {
      containerEl
        ?.querySelector('[aria-selected="true"]')
        ?.scrollIntoView({ block: "nearest" });
    });
  }

  function moveSelection(delta: number) {
    if (visibleRows.length === 0) return;
    selectedIndex = Math.min(Math.max(selectedIndex + delta, 0), visibleRows.length - 1);
    cancelConfirm();
    scrollSelectedIntoView();
  }

  function requestDelete(entry: Entry) {
    if (!$prefs.confirmDelete) {
      doDelete(entry);
      return;
    }
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
    const match = visibleRows.findIndex((r) => r.entry.name.toLowerCase().startsWith(typeahead));
    if (match >= 0) {
      selectedIndex = match;
      scrollSelectedIntoView();
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (renamingPath) return;
    const selected = visibleRows[selectedIndex]?.entry ?? null;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      moveSelection(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      moveSelection(-1);
    } else if (e.key === "ArrowRight" && selected?.kind === "dir" && !expanded.has(selected.path)) {
      e.preventDefault();
      toggleExpand(selected);
    } else if (e.key === "ArrowLeft" && selected?.kind === "dir" && expanded.has(selected.path)) {
      e.preventDefault();
      toggleExpand(selected);
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
    } else if (e.key === " " && typeahead === "" && selected && selected.kind !== "dir") {
      // Space previews the selected file ONLY when the type-ahead buffer is empty.
      // With a non-empty buffer (or a dir selected) Space falls through to the
      // catch-all below so names containing spaces still type-ahead.
      e.preventDefault();
      onpreview?.(selected);
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
  oncontextmenu={openEmptyMenu}
>
  <div class="header" role="presentation" oncontextmenu={openHeaderMenu}>
    <button class="col-name sort-btn" onclick={() => setSort("name")}>
      Name
      {#if sortCol === "name"}<span class="arr">{sortDir === 1 ? "▲" : "▼"}</span>{/if}
      {#if opening}<span class="opening">· Opening…</span>{/if}
    </button>
    {#if $prefs.columns.size}
      <div
        class="resize-handle"
        role="separator"
        aria-label="Resize Size column"
        onpointerdown={(e) => startColResize(e, "size")}
      ></div>
      <button
        class="col-size sort-btn"
        style="width:{$prefs.colWidths.size}px"
        onclick={() => setSort("size")}
      >
        {#if sortCol === "size"}<span class="arr">{sortDir === 1 ? "▲" : "▼"}</span>{/if}
        Size
      </button>
    {/if}
    {#if $prefs.columns.modified}
      <div
        class="resize-handle"
        role="separator"
        aria-label="Resize Modified column"
        onpointerdown={(e) => startColResize(e, "modified")}
      ></div>
      <button
        class="col-mtime sort-btn"
        style="width:{$prefs.colWidths.modified}px"
        onclick={() => setSort("mtime")}
      >
        {#if sortCol === "mtime"}<span class="arr">{sortDir === 1 ? "▲" : "▼"}</span>{/if}
        Modified
      </button>
    {/if}
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
    {#each visibleRows as { entry, depth }, i (entry.path)}
      <div
        class="row"
        class:selected={selectedIndex === i}
        role="option"
        aria-selected={selectedIndex === i}
        tabindex="-1"
        style="padding-left: {12 + depth * 15}px"
        onclick={() => {
          selectedIndex = i;
          cancelConfirm();
          // Keep DOM focus on the container so keyboard flow continues
          // without needing a second click (focus invariant).
          containerEl?.focus();
        }}
        ondblclick={() => { open(entry); containerEl?.focus(); }}
        oncontextmenu={(e) => openRowMenu(e, i, entry)}
        onkeydown={() => {}}
      >
        <span class="col-name">
          {#if entry.kind === "dir"}
            <button
              class="caret"
              title={expanded.has(entry.path) ? "Collapse" : "Expand"}
              aria-label={expanded.has(entry.path) ? "Collapse" : "Expand"}
              onclick={(e) => {
                e.stopPropagation();
                toggleExpand(entry);
              }}
            >
              <Icon name={expanded.has(entry.path) ? "chevron-down" : "chevron-right"} size={14} />
            </button>
          {:else}
            <span class="caret-spacer" aria-hidden="true"></span>
          {/if}
          <span class="glyph" class:dir={entry.kind === "dir"} aria-hidden="true">
            <Icon name={iconForEntry(entry)} size={16} />
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
          {#if $editPaths.has(entry.path)}
            <span class="editing-dot" title="Open for editing" aria-label="Open for editing"></span>
          {/if}
          {#if confirmingDeletePath === entry.path}
            <span class="confirm">Delete? Press again to confirm</span>
          {/if}
        </span>
        {#if $prefs.columns.size}
          <span class="col-gap" aria-hidden="true"></span>
          <span class="col-size" style="width:{$prefs.colWidths.size}px">{formatSize(entry.size)}</span>
        {/if}
        {#if $prefs.columns.modified}
          <span class="col-gap" aria-hidden="true"></span>
          <span class="col-mtime" style="width:{$prefs.colWidths.modified}px"
            >{formatMtime(entry.modifiedMs)}</span
          >
        {/if}
      </div>
    {/each}
  {/if}
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}

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
  .sort-btn {
    display: flex;
    align-items: center;
    gap: 3px;
    background: transparent;
    border: none;
    padding: 0;
    font: inherit;
    color: inherit;
    cursor: default;
  }
  .col-size.sort-btn,
  .col-mtime.sort-btn {
    justify-content: flex-end;
  }
  .sort-btn:hover {
    color: var(--fg-primary);
  }
  .arr {
    font-size: 8px;
    line-height: 1;
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
    gap: 5px;
    min-width: 0;
  }
  .caret {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    padding: 0;
    background: transparent;
    border: none;
    border-radius: 3px;
    color: var(--fg-secondary);
  }
  .caret:hover {
    background: var(--bg-hover);
    color: var(--fg-primary);
  }
  .caret-spacer {
    flex-shrink: 0;
    width: 16px;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .glyph {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--fg-secondary);
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
  /* Draggable divider in the header; the rows use an inert .col-gap of the same
     width so header and row columns stay pixel-aligned. */
  .resize-handle {
    flex-shrink: 0;
    width: 6px;
    align-self: stretch;
    cursor: col-resize;
  }
  .resize-handle:hover {
    background: var(--border);
  }
  .col-gap {
    flex-shrink: 0;
    width: 6px;
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
  .editing-dot {
    flex-shrink: 0;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
  }
  .opening {
    margin-left: 6px;
    color: var(--accent);
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
