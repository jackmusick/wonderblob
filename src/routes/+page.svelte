<div class="shell">
  <aside class="sidebar">
    <div class="sidebar-section">Connections</div>
    <!-- BookmarkList mounts here (Task 9) -->
  </aside>
  <main class="content">
    <div class="toolbar">
      <!-- breadcrumb + actions mount here (Task 10) -->
    </div>
    <div class="browser">
      <!-- FileList mounts here (Task 10) -->
    </div>
  </main>
</div>

<style>
  .shell { display: flex; height: 100vh; }
  .sidebar {
    width: var(--sidebar-width);
    background: var(--bg-sidebar);
    border-right: 1px solid var(--border);
    padding: 8px;
    flex-shrink: 0;
  }
  .sidebar-section {
    font-size: var(--text-small);
    font-weight: 600;
    color: var(--fg-secondary);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 6px 8px;
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
</style>
