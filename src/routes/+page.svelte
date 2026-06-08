<script lang="ts">
  import { open, save } from "@tauri-apps/plugin-dialog";
  import { writeText } from "@tauri-apps/plugin-clipboard-manager";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { api, type Bookmark, type Entry } from "$lib/api";
  import { describeError } from "$lib/errors";
  import BookmarkList from "$lib/components/BookmarkList.svelte";
  import Breadcrumb from "$lib/components/Breadcrumb.svelte";
  import ConflictModal from "$lib/components/ConflictModal.svelte";
  import ConnectionSheet from "$lib/components/ConnectionSheet.svelte";
  import ContextMenu, { type MenuItem } from "$lib/components/ContextMenu.svelte";
  import EditSessions from "$lib/components/EditSessions.svelte";
  import FileList from "$lib/components/FileList.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import PreviewOverlay from "$lib/components/PreviewOverlay.svelte";
  import TransfersPanel from "$lib/components/TransfersPanel.svelte";
  import { activeConnection, currentPath } from "$lib/stores/session";
  import { editConflicts, initEdit } from "$lib/stores/edit";
  import { activeTransferCount, initTransfers, transferList } from "$lib/stores/transfers";

  let previewEntry = $state<Entry | null>(null);

  let sheetOpen = $state(false);
  let editing = $state<Bookmark | null>(null);
  let list = $state<BookmarkList | null>(null);
  let fileList = $state<FileList | null>(null);

  let newFolderOpen = $state(false);
  let newFolderName = $state("");
  let newFolderInput = $state<HTMLInputElement | null>(null);
  let uploading = $state(false);

  let transfersOpen = $state(false);

  // Resizable sidebar (persisted; defaults wider than the old fixed 240px).
  let sidebarWidth = $state(280);
  $effect(() => {
    const saved = Number(localStorage.getItem("wb:sidebarWidth"));
    if (saved >= 180 && saved <= 600) sidebarWidth = saved;
  });
  function startSidebarResize(e: PointerEvent) {
    e.preventDefault();
    const startX = e.clientX;
    const startW = sidebarWidth;
    const onMove = (ev: PointerEvent) => {
      sidebarWidth = Math.min(600, Math.max(180, startW + (ev.clientX - startX)));
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      localStorage.setItem("wb:sidebarWidth", String(Math.round(sidebarWidth)));
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  }

  // Right-click menu for the Download toolbar icon (primary click = ~/Downloads).
  let downloadMenu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null);
  function openDownloadMenu(e: MouseEvent) {
    e.preventDefault();
    downloadMenu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: "Download to ~/Downloads", icon: "download", action: downloadToDownloads },
        { label: "Download As…", icon: "download", action: download },
      ],
    };
  }

  // True while OS files are dragged over the window — drives the drop highlight.
  let dragOver = $state(false);

  // Drag-in: OS file-manager drops arrive via the webview's native drag-drop
  // event (DOM `ondrop` does NOT fire — Tauri intercepts natively; see
  // tauri-apps/tauri#14373). The payload carries filesystem paths, which we hand
  // to enqueue_dropped → the tested upload path, into the current directory.
  $effect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent(async (e) => {
      const p = e.payload;
      if (p.type === "enter" || p.type === "over") {
        dragOver = true;
        return;
      }
      if (p.type === "leave") {
        dragOver = false;
        return;
      }
      // p.type === "drop"
      dragOver = false;
      const conn = $activeConnection;
      if (!conn) return;
      if (!p.paths || p.paths.length === 0) return;
      try {
        await api.enqueueDropped(conn.id, $currentPath, p.paths);
        transfersOpen = true; // reveal progress
      } catch (err) {
        showToast(opError(err, "Couldn't upload dropped files"));
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  });

  // Start the transfers store once (reconcile + subscribe to events).
  $effect(() => {
    initTransfers();
  });

  // Start the edit store once (reconcile + subscribe to edit:// events).
  $effect(() => {
    initEdit({
      onSaved: (name) => showToast(`Saved “${name}”`),
      onError: showToast,
    });
  });

  // Refresh the listing when an upload completes (it may be in the current dir).
  let seenCompleted = new Set<number>();
  $effect(() => {
    for (const t of $transferList) {
      if (t.direction === "up" && t.status === "completed" && !seenCompleted.has(t.id)) {
        seenCompleted.add(t.id);
        fileList?.refresh();
      }
    }
  });

  let toast = $state<string | null>(null);
  let toastTimer: ReturnType<typeof setTimeout> | null = null;

  let copied = $state<string | null>(null);
  let copiedTimer: ReturnType<typeof setTimeout> | null = null;

  function openNew() {
    editing = null;
    sheetOpen = true;
  }
  function openEdit(b: Bookmark) {
    editing = b;
    sheetOpen = true;
  }
  function closeSheet() {
    sheetOpen = false;
    editing = null;
  }
  async function onSaved() {
    closeSheet();
    await list?.reload();
  }

  function showToast(message: string) {
    toast = message;
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = null), 5000);
  }

  function opError(e: unknown, fallback: string): string {
    const msg = describeError(e, "mutate");
    return `${fallback}: ${msg.replace(/^Operation failed[.: ]*/, "").trim() || "unexpected error."}`;
  }

  function joinPath(dir: string, name: string): string {
    return dir === "/" ? `/${name}` : `${dir}/${name}`;
  }

  async function upload() {
    const conn = $activeConnection;
    if (!conn || uploading) return;
    const selected = await open({ multiple: true, directory: false, title: "Upload files" });
    const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
    if (paths.length === 0) return;
    uploading = true;
    try {
      for (const p of paths) {
        const base = p.replace(/[\\/]+$/, "").split(/[\\/]/).pop();
        if (!base) continue;
        await api.enqueueUpload(conn.id, p, joinPath($currentPath, base));
      }
      transfersOpen = true; // reveal progress
    } catch (e) {
      showToast(opError(e, "Couldn't start upload"));
    } finally {
      uploading = false;
    }
  }

  function openNewFolder() {
    newFolderName = "";
    newFolderOpen = true;
  }

  $effect(() => {
    if (newFolderOpen && newFolderInput) newFolderInput.focus();
  });

  async function commitNewFolder() {
    const conn = $activeConnection;
    const name = newFolderName.trim();
    newFolderOpen = false;
    if (!conn || !name) return;
    try {
      await api.makeDir(conn.id, joinPath($currentPath, name));
      fileList?.refresh();
    } catch (e) {
      showToast(opError(e, `Couldn't create “${name}”`));
    }
  }

  function newFolderKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      commitNewFolder();
    } else if (e.key === "Escape") {
      e.preventDefault();
      newFolderOpen = false;
    }
  }

  async function shareSelected() {
    const conn = $activeConnection;
    if (!conn) return;
    const entry = fileList?.selected() ?? null;
    if (!entry || entry.kind === "dir") {
      showToast("Select a file to share.");
      return;
    }
    try {
      const url = await api.shareLink(conn.id, entry.path, 24 * 60 * 60);
      await writeText(url);
      copied = "Link copied to clipboard";
      if (copiedTimer) clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => (copied = null), 2500);
    } catch (e) {
      showToast(opError(e, "Couldn't create share link"));
    }
  }

  async function download() {
    const conn = $activeConnection;
    if (!conn) return;
    const entry = fileList?.selected() ?? null;
    if (!entry || entry.kind === "dir") {
      showToast("Select a file to download.");
      return;
    }
    const dest = await save({ defaultPath: entry.name, title: "Download to…" });
    if (!dest) return;
    try {
      await api.enqueueDownload(conn.id, entry.path, dest, entry.size ?? undefined);
      transfersOpen = true; // reveal progress
    } catch (e) {
      showToast(opError(e, "Couldn't start download"));
    }
  }

  // Drag-out fallback: one-click download of the selected file straight to
  // ~/Downloads (no save dialog). Complements the save-dialog Download button.
  async function downloadToDownloads() {
    const conn = $activeConnection;
    if (!conn) return;
    const entry = fileList?.selected() ?? null;
    if (!entry || entry.kind === "dir") {
      showToast("Select a file to download.");
      return;
    }
    try {
      await api.enqueueDownloadToDownloads(conn.id, entry.path, entry.size ?? undefined);
      transfersOpen = true; // reveal progress
      showToast(`Downloading “${entry.name}” to Downloads`);
    } catch (e) {
      showToast(opError(e, "Couldn't start download"));
    }
  }

  async function disconnect() {
    const conn = $activeConnection;
    if (!conn) return;
    activeConnection.set(null);
    currentPath.set("/");
    api.disconnect(conn.id).catch(() => {});
  }
</script>

<div class="shell">
  <aside class="sidebar" style="width:{sidebarWidth}px">
    <BookmarkList bind:this={list} onnew={openNew} onedit={openEdit} />
  </aside>
  <div
    class="splitter"
    role="separator"
    aria-orientation="vertical"
    aria-label="Resize sidebar"
    tabindex="-1"
    onpointerdown={startSidebarResize}
  ></div>
  <main class="content">
    {#if $activeConnection}
      <div class="toolbar">
        <Breadcrumb />
        <div class="actions">
          {#if newFolderOpen}
            <input
              class="folder-input"
              bind:this={newFolderInput}
              bind:value={newFolderName}
              placeholder="Folder name"
              aria-label="New folder name"
              onkeydown={newFolderKeydown}
              onblur={() => (newFolderOpen = false)}
            />
          {:else}
            <button class="icon-btn" title="New folder" aria-label="New folder" onclick={openNewFolder}>
              <Icon name="folder-plus" />
            </button>
          {/if}
          <button
            class="icon-btn"
            title="Upload files"
            aria-label="Upload files"
            onclick={upload}
            disabled={uploading}
          >
            <Icon name="upload" />
          </button>
          <button
            class="icon-btn"
            title="Download to ~/Downloads (right-click for Download As…)"
            aria-label="Download"
            onclick={downloadToDownloads}
            oncontextmenu={openDownloadMenu}
          >
            <Icon name="download" />
          </button>
          {#if $activeConnection?.capabilities.canPresign}
            <button class="icon-btn" title="Copy share link" aria-label="Share link" onclick={shareSelected}>
              <Icon name="share" />
            </button>
          {/if}
          <span class="sep"></span>
          <EditSessions />
          <button
            class="icon-btn"
            class:on={transfersOpen}
            title="Transfers"
            aria-label="Transfers"
            onclick={() => (transfersOpen = !transfersOpen)}
          >
            <Icon name="transfers" />
            {#if $activeTransferCount > 0}<span class="badge">{$activeTransferCount}</span>{/if}
          </button>
          <button class="icon-btn" title="Disconnect" aria-label="Disconnect" onclick={disconnect}>
            <Icon name="power" />
          </button>
        </div>
      </div>
      <div class="browser" class:drop-target={dragOver}>
        <FileList
          bind:this={fileList}
          onerror={showToast}
          onpreview={(e) => (previewEntry = e)}
          onDownload={downloadToDownloads}
          onDownloadAs={download}
          onShare={shareSelected}
          onNewFolder={openNewFolder}
          onUpload={upload}
        />
        {#if previewEntry && $activeConnection}
          <PreviewOverlay
            entry={previewEntry}
            connectionId={$activeConnection.id}
            onclose={() => (previewEntry = null)}
            onopen={(e) => fileList?.openEntry(e)}
          />
        {/if}
      </div>
      {#if transfersOpen}
        <div class="transfers">
          <TransfersPanel onerror={showToast} onclose={() => (transfersOpen = false)} />
        </div>
      {/if}
      {#if toast}
        <div class="toast" role="alert">{toast}</div>
      {/if}
      {#if copied}
        <div class="copied" role="status">{copied}</div>
      {/if}
    {:else}
      <div class="empty">
        <p>Connect to a server to get started</p>
        <button class="primary" onclick={openNew}>New Connection</button>
      </div>
    {/if}
  </main>
</div>

{#if sheetOpen}
  <ConnectionSheet bookmark={editing} onclose={closeSheet} onsaved={onSaved} />
{/if}

{#if $editConflicts.length > 0}
  <ConflictModal session={$editConflicts[0]} />
{/if}

{#if downloadMenu}
  <ContextMenu
    x={downloadMenu.x}
    y={downloadMenu.y}
    items={downloadMenu.items}
    onclose={() => (downloadMenu = null)}
  />
{/if}

<style>
  .shell { display: flex; height: 100vh; }
  .sidebar {
    background: var(--bg-sidebar);
    padding: 8px;
    flex-shrink: 0;
    overflow-y: auto;
  }
  /* Drag handle between sidebar and content. */
  .splitter {
    width: 5px;
    margin-left: -3px;
    flex-shrink: 0;
    cursor: col-resize;
    background: var(--border);
    background-clip: content-box;
    border-left: 2px solid transparent;
    z-index: 5;
  }
  .splitter:hover {
    border-left-color: var(--accent);
  }
  .content { flex: 1; display: flex; flex-direction: column; background: var(--bg-content); min-width: 0; }
  .toolbar {
    height: 44px;
    border-bottom: 1px solid var(--border);
    display: flex;
    align-items: center;
    padding: 0 12px;
    gap: 8px;
    flex-shrink: 0;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
  }
  .icon-btn {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    color: var(--fg-secondary);
    background: transparent;
    border: none;
    border-radius: var(--radius);
    flex-shrink: 0;
  }
  .icon-btn:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg-primary);
  }
  .icon-btn.on {
    background: var(--bg-selected);
    color: var(--accent);
  }
  .icon-btn:disabled {
    opacity: 0.55;
  }
  .badge {
    position: absolute;
    top: -2px;
    right: -2px;
    min-width: 14px;
    height: 14px;
    padding: 0 3px;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 9px;
    font-weight: 600;
    color: #fff;
    background: var(--accent);
    border-radius: 7px;
  }
  .actions .sep {
    width: 1px;
    height: 18px;
    margin: 0 2px;
    background: var(--border);
  }
  .folder-input {
    width: 160px;
    height: 24px;
    padding: 0 7px;
    font-size: var(--text-base);
    font-family: var(--font-ui);
    color: var(--fg-primary);
    background: var(--bg-content);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    outline: none;
  }
  .browser { flex: 1; overflow-y: auto; position: relative; }
  /* Drop-target highlight while OS files are dragged over the pane. */
  .browser.drop-target::after {
    content: "";
    position: absolute;
    inset: 4px;
    border: 2px dashed var(--accent);
    border-radius: var(--radius);
    background: var(--bg-selected);
    opacity: 0.5;
    pointer-events: none;
    z-index: 2;
  }
  .transfers {
    flex-shrink: 0;
    max-height: 38%;
    display: flex;
    border-top: 1px solid var(--border);
    background: var(--bg-content);
  }
  .toast {
    flex-shrink: 0;
    padding: 6px 12px;
    font-size: var(--text-small);
    color: var(--danger);
    border-top: 1px solid var(--border);
    background: var(--bg-content);
  }
  .copied {
    flex-shrink: 0;
    padding: 6px 12px;
    font-size: var(--text-small);
    color: var(--fg-primary);
    border-top: 1px solid var(--border);
    background: var(--bg-selected);
  }
  .empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
  }
  .empty p {
    margin: 0;
    font-size: var(--text-base);
    color: var(--fg-secondary);
  }
  .empty .primary {
    height: 28px;
    padding: 0 14px;
    font-size: var(--text-base);
    font-family: var(--font-ui);
    color: #fff;
    background: var(--accent);
    border: none;
    border-radius: var(--radius);
  }
  .empty .primary:hover {
    filter: brightness(1.08);
  }
</style>
