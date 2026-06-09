<script lang="ts">
  import { untrack } from "svelte";
  import type { AuthMethod, AzAuthKind, Bookmark, Protocol } from "../api";
  import { api } from "../api";
  import { describeError } from "../errors";
  import { activeConnection, currentPath } from "../stores/session";

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

  // OneDrive (OAuth-driven; no secret field). The bookmark UUID must be stable
  // across save + sign-in because the OAuth command stores the refresh token in
  // the keychain keyed by this id. Editing reuses the saved id.
  const odBookmarkId = untrack(() => initial?.id ?? crypto.randomUUID());
  let odClientId = $state(initial?.onedrive?.clientIdOverride ?? "");
  let odAccountLabel = $state(initial?.onedrive?.accountLabel ?? "");
  let odAdvancedOpen = $state(!!initial?.onedrive?.clientIdOverride);
  let signingIn = $state(false);

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
  // OneDrive is OAuth-driven: no secret field renders.
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
    if (protocol === "oneDrive") {
      // Sign-in is the real gate; saving metadata only needs a label.
      return label.trim().length > 0;
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
    if (protocol === "oneDrive") {
      return {
        id,
        label: label.trim() || "OneDrive for Business",
        protocol: "oneDrive",
        onedrive: {
          clientIdOverride: odClientId.trim() || null,
          accountLabel: odAccountLabel.trim() || null,
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

  function newId(): string {
    // OneDrive reuses a stable id (refresh token is keyed by it in the keychain);
    // other protocols mint a fresh id for a new bookmark.
    if (protocol === "oneDrive") return odBookmarkId;
    return bookmark?.id ?? crypto.randomUUID();
  }

  async function save() {
    if (!valid() || saving) return;
    saving = true;
    error = null;
    const b = buildBookmark(newId());
    try {
      await api.bookmarkSave(b, secret || undefined);
      secret = ""; // clear secret from local state immediately
      onsaved(b);
    } catch (e) {
      error = (e as { detail?: string })?.detail ?? "Couldn't save bookmark";
      saving = false;
    }
  }

  // OneDrive primary action: persist the bookmark metadata FIRST (so the
  // refresh token the OAuth command writes lands under a known UUID), then run
  // the interactive browser sign-in, then re-save with the discovered account
  // label and activate the connection. A cancelled / failed sign-in surfaces an
  // error but leaves the saved bookmark in place so the user can retry.
  async function signInWithMicrosoft() {
    if (!valid() || signingIn) return;
    signingIn = true;
    error = null;
    try {
      // 1. Create/update the bookmark to reserve its UUID (no secret).
      await api.bookmarkSave(buildBookmark(odBookmarkId), undefined);
      // 2. Interactive OAuth in the system browser; backend persists the
      //    refresh token in the keychain under odBookmarkId.
      const res = await api.connectOnedrive(odBookmarkId, odClientId.trim() || null);
      odAccountLabel = res.accountLabel ?? "";
      // 3. Persist the discovered account label onto the bookmark.
      const b = buildBookmark(odBookmarkId);
      await api.bookmarkSave(b, undefined);
      // 4. Activate the live connection and notify the parent.
      activeConnection.set({ id: res.id, bookmark: b, capabilities: res.capabilities });
      currentPath.set(b.initialPath ?? "/");
      onsaved(b);
    } catch (e) {
      error = describeError(e, "list");
      signingIn = false;
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
      if (protocol === "oneDrive") signInWithMicrosoft();
      else save();
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
        <option value="oneDrive">Microsoft OneDrive</option>
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
    {:else if protocol === "oneDrive"}
      {#if odAccountLabel}
        <div class="field">
          <span>Account</span>
          <div class="readonly">Signed in as {odAccountLabel}</div>
        </div>
      {/if}
      <button
        type="button"
        class="advanced-toggle"
        aria-expanded={odAdvancedOpen}
        onclick={() => (odAdvancedOpen = !odAdvancedOpen)}
      >
        {odAdvancedOpen ? "▾" : "▸"} Advanced
      </button>
      {#if odAdvancedOpen}
        <label class="field">
          <span>Custom client ID (optional — leave blank for the default)</span>
          <input bind:value={odClientId} spellcheck="false" autocapitalize="off" placeholder="App registration GUID" />
        </label>
      {/if}
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
      <input
        bind:value={initialPath}
        placeholder={protocol === "sftp" ? "/var/www" : protocol === "oneDrive" ? "/Documents" : "/bucket"}
        spellcheck="false"
      />
    </label>

    {#if error}
      <div class="error">{error}</div>
    {/if}

    <div class="actions">
      <button class="ghost" onclick={onclose}>Cancel</button>
      {#if protocol === "oneDrive"}
        <button
          class="primary"
          onclick={signInWithMicrosoft}
          disabled={!valid() || signingIn}
        >
          {signingIn
            ? "Signing in…"
            : odAccountLabel
              ? "Re-sign in"
              : "Sign in with Microsoft"}
        </button>
      {:else}
        <button class="primary" onclick={save} disabled={!valid() || saving}>
          {saving ? "Saving…" : "Save"}
        </button>
      {/if}
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
    background: var(--bg-elevated);
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    box-shadow: 0 16px 44px rgba(0, 0, 0, 0.45);
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
    background: var(--bg-field);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    outline: none;
  }
  input:focus,
  select:focus {
    border-color: var(--accent);
  }
  .readonly {
    height: 28px;
    display: flex;
    align-items: center;
    padding: 0 8px;
    font-size: var(--text-base);
    color: var(--fg-primary);
    background: var(--bg-field);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .advanced-toggle {
    align-self: flex-start;
    height: auto;
    padding: 2px 0;
    background: transparent;
    border: none;
    color: var(--fg-secondary);
    font-size: var(--text-small);
    cursor: pointer;
  }
  .advanced-toggle:hover {
    color: var(--fg-primary);
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
