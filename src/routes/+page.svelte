<script lang="ts">
  import type { Bookmark } from "$lib/api";
  import BookmarkList from "$lib/components/BookmarkList.svelte";
  import ConnectionSheet from "$lib/components/ConnectionSheet.svelte";
  import { activeConnection } from "$lib/stores/session";

  let sheetOpen = $state(false);
  let editing = $state<Bookmark | null>(null);
  let list = $state<BookmarkList | null>(null);

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
</script>

<div class="shell">
  <aside class="sidebar">
    <BookmarkList bind:this={list} onnew={openNew} onedit={openEdit} />
  </aside>
  <main class="content">
    {#if $activeConnection}
      <div class="toolbar">
        <!-- breadcrumb + actions mount here (Task 10) -->
      </div>
      <div class="browser">
        <!-- FileList mounts here (Task 10) -->
      </div>
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
  .content { flex: 1; display: flex; flex-direction: column; background: var(--bg-content); }
  .toolbar {
    height: 44px;
    border-bottom: 1px solid var(--border);
    display: flex;
    align-items: center;
    padding: 0 12px;
    gap: 8px;
    flex-shrink: 0;
  }
  .browser { flex: 1; overflow-y: auto; }
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
    cursor: pointer;
  }
  .empty .primary:hover {
    filter: brightness(1.08);
  }
</style>
