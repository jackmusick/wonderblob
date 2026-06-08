<script lang="ts">
  import { untrack } from "svelte";
  import type { AuthMethod, AzAuthKind, Bookmark, Protocol } from "../api";
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

  let protocol = $state<Protocol>(initial?.protocol ?? "sftp");
  let label = $state(initial?.label ?? "");
  let initialPath = $state(initial?.initialPath ?? "");

  // SFTP
  let host = $state(initial?.host ?? "");
  let port = $state(initial?.port ?? 22);
  let username = $state(initial?.username ?? "");
  let authType = $state<AuthMethod["type"]>(initial?.authMethod?.type ?? "agent");
  let keyPath = $state(initial?.authMethod?.type === "keyFile" ? initial.authMethod.path : "");

  // S3
  let s3AccessKeyId = $state(initial?.s3?.accessKeyId ?? "");
  let s3Region = $state(initial?.s3?.region ?? "");
  let s3Endpoint = $state(initial?.s3?.endpoint ?? "");
  let s3ForcePathStyle = $state(initial?.s3?.forcePathStyle ?? false);

  // Azure Blob
  let azAccount = $state(initial?.azblob?.account ?? "");
  let azEndpoint = $state(initial?.azblob?.endpoint ?? "");
  let azAuthKind = $state<AzAuthKind>(initial?.azblob?.authKind ?? "accountKey");

  // Single secret slot; meaning depends on protocol/auth. Never persisted locally.
  let secret = $state("");
  let saving = $state(false);
  let error = $state<string | null>(null);
  let firstInput = $state<HTMLInputElement | null>(null);
  let panelEl = $state<HTMLDivElement | null>(null);

  // Editing the same protocol that already stored a secret: blank means keep.
  // Reactive so switching the picker away from the saved protocol drops the hint
  // (there is no saved secret for the newly-selected protocol).
  const editingSameProto = $derived(initial != null && initial.protocol === protocol);
  const protoUsesSecret = $derived(
    protocol === "s3" ||
      protocol === "azBlob" ||
      (protocol === "sftp" && authType !== "agent")
  );
  let secretPlaceholder = $derived(
    editingSameProto && protoUsesSecret ? "Leave blank to keep saved secret" : ""
  );

  $effect(() => {
    firstInput?.focus();
  });

  function secretRequired(): boolean {
    if (!protoUsesSecret) return false;
    if (protocol === "sftp" && authType === "keyFile") return false; // passphrase optional
    return !editingSameProto; // required for new; optional when editing same proto
  }

  function valid(): boolean {
    if (secretRequired() && !secret) return false;
    if (protocol === "sftp") {
      if (!host.trim() || !username.trim()) return false;
      if (authType === "keyFile" && !keyPath.trim()) return false;
      return port >= 1 && port <= 65535;
    }
    if (protocol === "s3") {
      return s3AccessKeyId.trim().length > 0;
    }
    // azBlob
    return azAccount.trim().length > 0;
  }

  function buildBookmark(id: string): Bookmark {
    if (protocol === "sftp") {
      const authMethod: AuthMethod =
        authType === "agent"
          ? { type: "agent" }
          : authType === "keyFile"
            ? { type: "keyFile", path: keyPath.trim() }
            : { type: "password" };
      return {
        id,
        label: label.trim() || host.trim(),
        protocol: "sftp",
        host: host.trim(),
        port,
        username: username.trim(),
        authMethod,
        initialPath: initialPath.trim() || null,
      };
    }
    if (protocol === "s3") {
      return {
        id,
        label: label.trim() || s3Endpoint.trim() || "Amazon S3",
        protocol: "s3",
        s3: {
          accessKeyId: s3AccessKeyId.trim(),
          region: s3Region.trim() || null,
          endpoint: s3Endpoint.trim() || null,
          forcePathStyle: s3ForcePathStyle,
        },
        initialPath: initialPath.trim() || "/",
      };
    }
    return {
      id,
      label: label.trim() || azAccount.trim() || "Azure Blob",
      protocol: "azBlob",
      azblob: {
        account: azAccount.trim(),
        endpoint: azEndpoint.trim() || null,
        authKind: azAuthKind,
      },
      initialPath: initialPath.trim() || "/",
    };
  }

  async function save() {
    if (!valid() || saving) return;
    saving = true;
    error = null;
    const b = buildBookmark(bookmark?.id ?? crypto.randomUUID());
    try {
      await api.bookmarkSave(b, secret || undefined);
      secret = ""; // clear secret from local state immediately
      onsaved(b);
    } catch (e) {
      error = (e as { detail?: string })?.detail ?? "Couldn't save bookmark";
      saving = false;
    }
  }

  const secretLabel = $derived(
    protocol === "s3"
      ? "Secret Access Key"
      : protocol === "azBlob"
        ? azAuthKind === "accountKey"
          ? "Account Key"
          : azAuthKind === "connectionString"
            ? "Connection String"
            : "SAS Token"
        : "Password"
  );

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onclose();
    } else if (e.key === "Tab") {
      // Trap focus inside the dialog: cycle first <-> last focusable.
      const focusables = Array.from(
        panelEl?.querySelectorAll<HTMLElement>(
          "input, select, button:not(:disabled)"
        ) ?? []
      );
      if (focusables.length === 0) return;
      const first = focusables[0];
      const last = focusables[focusables.length - 1];
      const target = e.target as HTMLElement;
      if (e.shiftKey && (target === first || !panelEl?.contains(target))) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && (target === last || !panelEl?.contains(target))) {
        e.preventDefault();
        first.focus();
      }
    } else if (
      e.key === "Enter" &&
      !(e.target instanceof HTMLButtonElement) &&
      !(e.target instanceof HTMLSelectElement)
    ) {
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
  <div class="panel" bind:this={panelEl}>
    <div class="title">{bookmark ? "Edit Connection" : "New Connection"}</div>

    <label class="field">
      <span>Protocol</span>
      <select bind:value={protocol}>
        <option value="sftp">SFTP</option>
        <option value="s3">Amazon S3 (and compatible)</option>
        <option value="azBlob">Azure Blob Storage</option>
      </select>
    </label>

    <label class="field">
      <span>Label</span>
      <input bind:this={firstInput} bind:value={label} placeholder="My connection" />
    </label>

    {#if protocol === "sftp"}
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
      {/if}
    {:else if protocol === "s3"}
      <label class="field">
        <span>Endpoint (optional — leave blank for AWS)</span>
        <input bind:value={s3Endpoint} placeholder="https://… (MinIO/Wasabi/R2)" spellcheck="false" />
      </label>
      <label class="field">
        <span>Region</span>
        <input bind:value={s3Region} placeholder="us-east-1" spellcheck="false" />
      </label>
      <label class="field">
        <span>Access Key ID</span>
        <input bind:value={s3AccessKeyId} spellcheck="false" autocapitalize="off" />
      </label>
      <label class="checkrow">
        <input type="checkbox" bind:checked={s3ForcePathStyle} />
        <span>Force path-style addressing (MinIO, most S3-compatible servers)</span>
      </label>
    {:else}
      <label class="field">
        <span>Account name</span>
        <input bind:value={azAccount} spellcheck="false" autocapitalize="off" placeholder="mystorageacct" />
      </label>
      <label class="field">
        <span>Endpoint (optional — leave blank for Azure)</span>
        <input bind:value={azEndpoint} placeholder="http://127.0.0.1:10000/devstoreaccount1" spellcheck="false" />
      </label>
      <label class="field">
        <span>Credential type</span>
        <select bind:value={azAuthKind}>
          <option value="accountKey">Account key</option>
          <option value="connectionString">Connection string</option>
          <option value="sas">SAS token</option>
        </select>
      </label>
    {/if}

    {#if protoUsesSecret}
      <label class="field">
        <span>{secretLabel}</span>
        <input type="password" bind:value={secret} autocomplete="off" placeholder={secretPlaceholder} />
      </label>
    {/if}

    <label class="field">
      <span>Initial path (optional)</span>
      <input bind:value={initialPath} placeholder={protocol === "sftp" ? "/var/www" : "/bucket"} spellcheck="false" />
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
  .checkrow {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--text-small);
    color: var(--fg-secondary);
  }
  .checkrow input {
    height: auto;
    width: auto;
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
    color: var(--danger);
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
  }
</style>
