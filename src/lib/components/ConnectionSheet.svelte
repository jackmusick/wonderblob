<script lang="ts">
  import { untrack } from "svelte";
  import type { AuthMethod, Bookmark } from "../api";
  import { api } from "../api";

  let {
    bookmark = null,
    onclose,
    onsaved,
  }: {
    bookmark?: Bookmark | null;
    onclose: () => void;
    onsaved: (b: Bookmark) => void;
  } = $props();

  // Snapshot the bookmark prop once: the sheet is remounted per open, so the
  // form intentionally captures initial values only.
  const initial = untrack(() => bookmark);
  let label = $state(initial?.label ?? "");
  let host = $state(initial?.host ?? "");
  let port = $state(initial?.port ?? 22);
  let username = $state(initial?.username ?? "");
  let authType = $state<AuthMethod["type"]>(initial?.authMethod.type ?? "agent");
  let keyPath = $state(initial?.authMethod.type === "keyFile" ? initial.authMethod.path : "");
  let secret = $state(""); // password or key passphrase; never persisted locally
  let initialPath = $state(initial?.initialPath ?? "");
  let saving = $state(false);
  let error = $state<string | null>(null);
  let firstInput = $state<HTMLInputElement | null>(null);

  $effect(() => {
    firstInput?.focus();
  });

  function valid(): boolean {
    if (!host.trim() || !username.trim()) return false;
    if (authType === "keyFile" && !keyPath.trim()) return false;
    return port >= 1 && port <= 65535;
  }

  async function save() {
    if (!valid() || saving) return;
    saving = true;
    error = null;
    const authMethod: AuthMethod =
      authType === "agent"
        ? { type: "agent" }
        : authType === "keyFile"
          ? { type: "keyFile", path: keyPath.trim() }
          : { type: "password" };
    const b: Bookmark = {
      id: bookmark?.id ?? crypto.randomUUID(),
      label: label.trim() || host.trim(),
      protocol: "sftp",
      host: host.trim(),
      port,
      username: username.trim(),
      authMethod,
      initialPath: initialPath.trim() || null,
    };
    try {
      await api.bookmarkSave(b, secret || undefined);
      secret = ""; // clear secret from local state immediately
      onsaved(b);
    } catch (e) {
      error = (e as { detail?: string })?.detail ?? "Couldn't save bookmark";
      saving = false;
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onclose();
    } else if (e.key === "Enter" && !(e.target instanceof HTMLButtonElement)) {
      e.preventDefault();
      save();
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  class="overlay"
  role="dialog"
  aria-modal="true"
  aria-label={bookmark ? "Edit connection" : "New connection"}
  tabindex="-1"
  onkeydown={onkeydown}
>
  <div class="backdrop" onclick={onclose} aria-hidden="true"></div>
  <div class="panel">
    <div class="title">{bookmark ? "Edit Connection" : "New Connection"}</div>

    <label class="field">
      <span>Label</span>
      <input bind:this={firstInput} bind:value={label} placeholder="My server" />
    </label>

    <div class="row">
      <label class="field grow">
        <span>Host</span>
        <input bind:value={host} placeholder="example.com" spellcheck="false" />
      </label>
      <label class="field port">
        <span>Port</span>
        <input type="number" bind:value={port} min="1" max="65535" />
      </label>
    </div>

    <label class="field">
      <span>Username</span>
      <input bind:value={username} spellcheck="false" autocapitalize="off" />
    </label>

    <label class="field">
      <span>Authentication</span>
      <select bind:value={authType}>
        <option value="agent">SSH Agent</option>
        <option value="keyFile">Key file</option>
        <option value="password">Password</option>
      </select>
    </label>

    {#if authType === "keyFile"}
      <label class="field">
        <span>Key file path</span>
        <input bind:value={keyPath} placeholder="~/.ssh/id_ed25519" spellcheck="false" />
      </label>
      <label class="field">
        <span>Key passphrase (optional)</span>
        <input type="password" bind:value={secret} autocomplete="off" />
      </label>
    {:else if authType === "password"}
      <label class="field">
        <span>Password</span>
        <input type="password" bind:value={secret} autocomplete="off" />
      </label>
    {/if}

    <label class="field">
      <span>Initial path (optional)</span>
      <input bind:value={initialPath} placeholder="/var/www" spellcheck="false" />
    </label>

    {#if error}
      <div class="error">{error}</div>
    {/if}

    <div class="actions">
      <button class="ghost" onclick={onclose}>Cancel</button>
      <button class="primary" onclick={save} disabled={!valid() || saving}>
        {saving ? "Saving…" : "Save"}
      </button>
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
    width: 420px;
    max-height: 85vh;
    overflow-y: auto;
    background: var(--bg-content);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.25);
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .title {
    font-size: var(--text-base);
    font-weight: 600;
    margin-bottom: 2px;
  }
  .row {
    display: flex;
    gap: 8px;
  }
  .grow {
    flex: 1;
  }
  .port {
    width: 76px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .field span {
    font-size: var(--text-small);
    color: var(--fg-secondary);
  }
  input,
  select {
    height: 28px;
    padding: 0 8px;
    font-size: var(--text-base);
    font-family: var(--font-ui);
    color: var(--fg-primary);
    background: var(--bg-app);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    outline: none;
  }
  input:focus,
  select:focus {
    border-color: var(--accent);
  }
  .error {
    font-size: var(--text-small);
    color: var(--accent);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }
  button {
    height: 28px;
    padding: 0 14px;
    font-size: var(--text-base);
    font-family: var(--font-ui);
    border-radius: var(--radius);
    border: 1px solid transparent;
    cursor: pointer;
  }
  button.ghost {
    background: transparent;
    color: var(--fg-primary);
    border-color: var(--border);
  }
  button.ghost:hover {
    background: var(--bg-hover);
  }
  button.primary {
    background: var(--accent);
    color: #fff;
  }
  button.primary:hover:not(:disabled) {
    filter: brightness(1.08);
  }
  button.primary:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
