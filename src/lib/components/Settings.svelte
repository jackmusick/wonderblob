<script lang="ts">
  import { prefs, type Theme } from "$lib/stores/prefs";

  let { onclose }: { onclose: () => void } = $props();

  const themes: { value: Theme; label: string }[] = [
    { value: "system", label: "System" },
    { value: "light", label: "Light" },
    { value: "dark", label: "Dark" },
  ];

  function setTheme(t: Theme) {
    prefs.update((p) => ({ ...p, theme: t }));
  }
  function toggleConfirmDelete() {
    prefs.update((p) => ({ ...p, confirmDelete: !p.confirmDelete }));
  }
  function toggleColumn(key: "size" | "modified") {
    prefs.update((p) => ({ ...p, columns: { ...p.columns, [key]: !p.columns[key] } }));
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onclose();
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  class="overlay"
  role="dialog"
  aria-modal="true"
  aria-label="Settings"
  tabindex="-1"
  onkeydown={onkeydown}
>
  <div class="backdrop" onclick={onclose} aria-hidden="true"></div>
  <div class="panel">
    <div class="title">Settings</div>

    <div class="field">
      <span class="flabel">Appearance</span>
      <div class="seg" role="group" aria-label="Theme">
        {#each themes as t}
          <button
            class="seg-btn"
            class:active={$prefs.theme === t.value}
            onclick={() => setTheme(t.value)}>{t.label}</button
          >
        {/each}
      </div>
    </div>

    <label class="checkrow">
      <input type="checkbox" checked={$prefs.confirmDelete} onchange={toggleConfirmDelete} />
      <span>Confirm before deleting</span>
    </label>

    <div class="field">
      <span class="flabel">File columns</span>
      <label class="checkrow">
        <input type="checkbox" checked={$prefs.columns.size} onchange={() => toggleColumn("size")} />
        <span>Size</span>
      </label>
      <label class="checkrow">
        <input
          type="checkbox"
          checked={$prefs.columns.modified}
          onchange={() => toggleColumn("modified")}
        />
        <span>Modified</span>
      </label>
    </div>

    <div class="actions">
      <button class="primary" onclick={onclose}>Done</button>
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
    z-index: 100;
  }
  .backdrop {
    position: absolute;
    inset: 0;
    background: rgba(0, 0, 0, 0.3);
  }
  .panel {
    position: relative;
    width: 360px;
    max-height: 85vh;
    overflow-y: auto;
    background: var(--bg-elevated);
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    box-shadow: 0 16px 44px rgba(0, 0, 0, 0.45);
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .title {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .flabel {
    font-size: var(--text-small);
    color: var(--fg-secondary);
  }
  .seg {
    display: flex;
    gap: 2px;
    padding: 2px;
    background: var(--bg-field);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    width: fit-content;
  }
  .seg-btn {
    height: 24px;
    padding: 0 14px;
    font-size: var(--text-base);
    font-family: var(--font-ui);
    color: var(--fg-secondary);
    background: transparent;
    border: none;
    border-radius: 4px;
  }
  .seg-btn:hover {
    color: var(--fg-primary);
  }
  /* Active segment = accent fill (vivid, 1Password/macOS-style) so it pops
     off the inset track on the dark panel. */
  .seg-btn.active {
    background: var(--accent);
    color: #fff;
  }
  .checkrow {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--text-base);
    color: var(--fg-primary);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    margin-top: 2px;
  }
  .primary {
    height: 28px;
    padding: 0 16px;
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
</style>
