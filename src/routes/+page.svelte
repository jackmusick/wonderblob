<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { api, type Bookmark } from "$lib/api";
  import { describeError } from "$lib/errors";
  import BookmarkList from "$lib/components/BookmarkList.svelte";
  import Breadcrumb from "$lib/components/Breadcrumb.svelte";
  import ConnectionSheet from "$lib/components/ConnectionSheet.svelte";
  import FileList from "$lib/components/FileList.svelte";
  import { activeConnection, currentPath } from "$lib/stores/session";

  let sheetOpen = $state(false);
  let editing = $state<Bookmark | null>(null);
  let list = $state<BookmarkList | null>(null);
  let fileList = $state<FileList | null>(null);

  let newFolderOpen = $state(false);
  let newFolderName = $state("");
  let newFolderInput = $state<HTMLInputElement | null>(null);
  let uploading = $state(false);

  let toast = $state<string | null>(null);
  let toastTimer: ReturnType<typeof setTimeout> | null = null;

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
    const selected = await open({ multiple: false, directory: false, title: "Upload file" });
    if (typeof selected !== "string") return;
    const basename = selected.replace(/\/+$/, "").split(/[\\/]/).pop();
    if (!basename) return;
    uploading = true;
    try {
      await api.uploadFile(conn.id, selected, joinPath($currentPath, basename));
      fileList?.refresh();
    } catch (e) {
      showToast(opError(e, `Couldn't upload “${basename}”`));
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

  async function disconnect() {
    const conn = $activeConnection;
    if (!conn) return;
    activeConnection.set(null);
    currentPath.set("/");
    api.disconnect(conn.id).catch(() => {});
  }
</script>

<div class="shell">
  <aside class="sidebar">
    <BookmarkList bind:this={list} onnew={openNew} onedit={openEdit} />
  </aside>
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
            <button class="ghost" onclick={openNewFolder}>New Folder</button>
          {/if}
          <button class="ghost" onclick={upload} disabled={uploading}>
            {uploading ? "Uploading…" : "Upload"}
          </button>
          <button class="ghost" onclick={disconnect}>Disconnect</button>
        </div>
      </div>
      <div class="browser">
        <FileList bind:this={fileList} onerror={showToast} />
      </div>
      {#if toast}
        <div class="toast" role="alert">{toast}</div>
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

<style>
  .shell { display: flex; height: 100vh; }
  .sidebar {
    width: var(--sidebar-width);
    background: var(--bg-sidebar);
    border-right: 1px solid var(--border);
    padding: 8px;
    flex-shrink: 0;
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
  .ghost {
    height: 24px;
    padding: 0 9px;
    font-size: var(--text-base);
    font-family: var(--font-ui);
    color: var(--fg-secondary);
    background: transparent;
    border: none;
    border-radius: var(--radius);
    white-space: nowrap;
  }
  .ghost:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg-primary);
  }
  .ghost:disabled {
    opacity: 0.55;
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
  .browser { flex: 1; overflow-y: auto; }
  .toast {
    flex-shrink: 0;
    padding: 6px 12px;
    font-size: var(--text-small);
    color: var(--danger);
    border-top: 1px solid var(--border);
    background: var(--bg-content);
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
