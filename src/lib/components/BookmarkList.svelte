<script lang="ts">
  import {
    api,
    type Bookmark,
    type StorageError,
    type HostKeyApproval,
    type SftpConnectResponse,
  } from "../api";
  import { activeConnection, currentPath } from "../stores/session";
  import HostKeyDialog from "./HostKeyDialog.svelte";
  import Icon from "./Icon.svelte";
  import ContextMenu, { type MenuItem } from "./ContextMenu.svelte";

  let {
    onnew,
    onedit,
  }: {
    onnew: () => void;
    onedit: (b: Bookmark) => void;
  } = $props();

  let bookmarks = $state<Bookmark[]>([]);
  let focusedIndex = $state(-1);
  let connectingId = $state<string | null>(null);
  let errors = $state<Record<string, { message: string; detail: string }>>({});
  let confirmingDeleteId = $state<string | null>(null);
  let confirmTimer: ReturnType<typeof setTimeout> | null = null;
  let menu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null);

  export async function reload() {
    bookmarks = await api.bookmarksList();
  }

  function protoIcon(p: Bookmark["protocol"]): string {
    switch (p) {
      case "sftp":
        return "sftp";
      case "s3":
        return "s3";
      case "azBlob":
        return "azure";
      case "oneDrive":
        return "onedrive";
    }
  }

  /** Tear down the active connection (mirrors the old toolbar Disconnect). */
  function disconnectActive() {
    const active = $activeConnection;
    if (!active) return;
    activeConnection.set(null);
    currentPath.set("/");
    api.disconnect(active.id).catch(() => {});
  }

  /** Single click toggles: connect, or disconnect if already the live one. */
  function toggle(b: Bookmark) {
    if ($activeConnection?.bookmark.id === b.id) disconnectActive();
    else connect(b);
  }

  function rowMenuItems(b: Bookmark): MenuItem[] {
    const isActive = $activeConnection?.bookmark.id === b.id;
    return [
      isActive
        ? { label: "Disconnect", icon: "power", action: () => disconnectActive() }
        : { label: "Connect", icon: "power", action: () => connect(b) },
      { label: "Edit…", icon: "pencil", action: () => onedit(b) },
      { separator: true },
      { label: "Delete", icon: "trash", danger: true, action: () => doDelete(b) },
    ];
  }

  function openRowMenu(e: MouseEvent, i: number, b: Bookmark) {
    e.preventDefault();
    e.stopPropagation();
    focusedIndex = i;
    menu = { x: e.clientX, y: e.clientY, items: rowMenuItems(b) };
  }

  function rowTitle(b: Bookmark): string {
    if (b.protocol === "sftp") return `${b.username ?? ""}@${b.host ?? ""}:${b.port ?? 22}`;
    if (b.protocol === "s3") return b.s3?.endpoint ?? `S3 (${b.s3?.region ?? "aws"})`;
    if (b.protocol === "oneDrive")
      return b.onedrive?.accountLabel ?? "OneDrive for Business";
    return b.azblob?.endpoint ?? `Azure (${b.azblob?.account ?? ""})`;
  }

  $effect(() => {
    reload();
  });

  function errorMessage(e: unknown, b?: Bookmark): { message: string; detail: string } {
    const err = e as StorageError;
    const detail = typeof err?.detail === "string" ? err.detail : String(e);
    switch (err?.kind) {
      case "authFailed":
        // OneDrive re-auth: the refresh token expired/was revoked. The user
        // re-opens the sheet to "Sign in with Microsoft" again.
        return {
          message:
            b?.protocol === "oneDrive"
              ? "Sign in again — open to re-authenticate"
              : "Authentication failed",
          detail,
        };
      case "network":
        return { message: "Can't reach server", detail };
      default:
        return { message: "Connection failed", detail };
    }
  }

  // Host-key approval dialog state. When an SFTP connect returns
  // `hostKeyUnverified`, we stash the details + a resolver here and await the
  // user's choice (accept-and-remember / accept-once / cancel) before retrying.
  type HostKeyUnverified = Extract<SftpConnectResponse, { kind: "hostKeyUnverified" }>;
  let hostKeyPrompt = $state<HostKeyUnverified | null>(null);
  let hostKeyResolve: ((approval: HostKeyApproval | null) => void) | null = null;

  function showHostKeyDialog(u: HostKeyUnverified): Promise<HostKeyApproval | null> {
    hostKeyPrompt = u;
    return new Promise((resolve) => {
      hostKeyResolve = resolve;
    });
  }

  function resolveHostKey(approval: HostKeyApproval | null) {
    const r = hostKeyResolve;
    hostKeyPrompt = null;
    hostKeyResolve = null;
    r?.(approval);
  }

  async function connect(b: Bookmark) {
    if (connectingId) return;
    connectingId = b.id;
    const { [b.id]: _, ...rest } = errors;
    errors = rest;
    try {
      let res = await api.connectBookmark(b.id);
      // Two-phase TOFU: an unverified SFTP host key opens the approval dialog,
      // then we retry once carrying the user's decision. Cloud bookmarks never
      // return `hostKeyUnverified`, so this branch is SFTP-only.
      if (res.kind === "hostKeyUnverified") {
        const approval = await showHostKeyDialog(res);
        if (!approval) return; // cancelled — abort quietly
        res = await api.connectBookmark(b.id, approval);
      }
      if (res.kind !== "connected") return;
      activeConnection.set({ id: res.id, bookmark: b, capabilities: res.capabilities });
      currentPath.set(b.initialPath ?? "/");
    } catch (e) {
      errors = { ...errors, [b.id]: errorMessage(e, b) };
    } finally {
      connectingId = null;
    }
  }

  function requestDelete(b: Bookmark) {
    if (confirmingDeleteId === b.id) {
      if (confirmTimer) clearTimeout(confirmTimer);
      confirmingDeleteId = null;
      doDelete(b);
    } else {
      confirmingDeleteId = b.id;
      if (confirmTimer) clearTimeout(confirmTimer);
      confirmTimer = setTimeout(() => (confirmingDeleteId = null), 3000);
    }
  }

  async function doDelete(b: Bookmark) {
    try {
      await api.bookmarkDelete(b.id);
      const active = $activeConnection;
      if (active?.bookmark.id === b.id) {
        api.disconnect(active.id).catch(() => {});
        activeConnection.set(null);
      }
      await reload();
      if (focusedIndex >= bookmarks.length) focusedIndex = bookmarks.length - 1;
    } catch (e) {
      errors = { ...errors, [b.id]: errorMessage(e) };
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (bookmarks.length === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      focusedIndex = Math.min(focusedIndex + 1, bookmarks.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      focusedIndex = Math.max(focusedIndex - 1, 0);
    } else if (e.key === "Enter" && focusedIndex >= 0) {
      e.preventDefault();
      connect(bookmarks[focusedIndex]);
    } else if (e.key === "Delete" && focusedIndex >= 0) {
      e.preventDefault();
      requestDelete(bookmarks[focusedIndex]);
    } else if (e.key === "F2" && focusedIndex >= 0) {
      // F2 only – matches desktop conventions; bare 'e' is not bound here.
      e.preventDefault();
      onedit(bookmarks[focusedIndex]);
    }
  }
</script>

<div class="section-header">
  <span class="section-label">Connections</span>
  <button class="icon-btn" title="New connection" aria-label="New connection" onclick={onnew}>
    <Icon name="plus" size={16} />
  </button>
</div>

<div
  class="list"
  role="listbox"
  aria-label="Saved connections"
  tabindex="0"
  onkeydown={onkeydown}
>
  {#each bookmarks as b, i (b.id)}
    {@const selected = $activeConnection?.bookmark.id === b.id}
    <div class="row-wrap">
      <div
        class="row"
        class:selected
        class:focused={focusedIndex === i}
        role="option"
        aria-selected={selected}
        tabindex="-1"
        onclick={() => {
          focusedIndex = i;
          toggle(b);
        }}
        oncontextmenu={(e) => openRowMenu(e, i, b)}
        onkeydown={() => {}}
      >
        <span class="proto" class:connected={selected} aria-hidden="true">
          <Icon name={protoIcon(b.protocol)} size={16} />
        </span>
        <span class="label" title={rowTitle(b)}>{b.label}</span>
        {#if connectingId === b.id}
          <span class="hint">connecting…</span>
        {:else}
          {#if selected}<span class="dot" title="Connected" aria-label="Connected"></span>{/if}
          <span class="row-actions">
            {#if selected}
              <button
                class="icon-btn"
                title="Disconnect"
                aria-label="Disconnect {b.label}"
                onclick={(e) => {
                  e.stopPropagation();
                  disconnectActive();
                }}><Icon name="power" size={15} /></button
              >
            {/if}
            <button
              class="icon-btn"
              title="Edit"
              aria-label="Edit {b.label}"
              onclick={(e) => {
                e.stopPropagation();
                onedit(b);
              }}><Icon name="pencil" size={15} /></button
            >
            <button
              class="icon-btn"
              class:confirming={confirmingDeleteId === b.id}
              title="Delete"
              aria-label="Delete {b.label}"
              onclick={(e) => {
                e.stopPropagation();
                requestDelete(b);
              }}
            >
              {#if confirmingDeleteId === b.id}Delete?{:else}<Icon name="trash" size={15} />{/if}
            </button>
          </span>
        {/if}
      </div>
      {#if errors[b.id]}
        <div class="error" title={errors[b.id].detail}>{errors[b.id].message}</div>
      {/if}
    </div>
  {/each}
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}

{#if hostKeyPrompt}
  <HostKeyDialog
    host={hostKeyPrompt.host}
    port={hostKeyPrompt.port}
    fingerprint={hostKeyPrompt.fingerprint}
    changed={hostKeyPrompt.changed}
    onaccept={(remember) =>
      resolveHostKey({ keyB64: hostKeyPrompt!.keyB64, remember })}
    oncancel={() => resolveHostKey(null)}
  />
{/if}

<style>
  .section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 8px;
  }
  .section-label {
    font-size: var(--text-small);
    font-weight: 600;
    color: var(--fg-secondary);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .list {
    outline: none;
    border-radius: var(--radius);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    height: var(--row-height);
    padding: 0 8px;
    border-radius: var(--radius);
    cursor: default;
    user-select: none;
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.selected {
    background: var(--bg-selected);
  }
  .list:focus .row.focused {
    outline: 1px solid var(--accent);
    outline-offset: -1px;
  }
  .proto {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    color: var(--fg-secondary);
  }
  .proto.connected {
    color: var(--accent);
  }
  .label {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    color: var(--fg-primary);
  }
  .hint {
    font-size: var(--text-small);
    color: var(--fg-secondary);
  }
  /* Green presence dot on the live connection; hidden while hover actions show. */
  .dot {
    flex-shrink: 0;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #3fb950;
  }
  .row:hover .dot,
  .row:focus-within .dot {
    display: none;
  }
  /* Hidden until hover or keyboard focus lands inside the row, but the
     buttons stay rendered so they remain in the tab order. */
  .row-actions {
    display: flex;
    gap: 2px;
    opacity: 0;
  }
  .row:hover .row-actions,
  .row:focus-within .row-actions {
    opacity: 1;
  }
  .icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 20px;
    height: 20px;
    padding: 0 3px;
    font-size: var(--text-base);
    font-family: var(--font-ui);
    color: var(--fg-secondary);
    background: transparent;
    border: none;
    border-radius: var(--radius);
  }
  .icon-btn:hover {
    background: var(--bg-hover);
    color: var(--fg-primary);
  }
  .icon-btn.confirming {
    color: var(--fg-primary);
    background: var(--bg-hover);
    font-size: var(--text-small);
    font-weight: 600;
  }
  .error {
    font-size: var(--text-small);
    color: var(--danger);
    padding: 1px 8px 4px;
  }
</style>
