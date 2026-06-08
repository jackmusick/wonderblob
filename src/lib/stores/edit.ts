import { writable, derived } from "svelte/store";
import { listen } from "@tauri-apps/api/event";
import { api, type EditSessionInfo } from "$lib/api";

export const editSessions = writable<EditSessionInfo[]>([]);

/** Remote paths currently open for edit — FileList uses this for the row badge. */
export const editPaths = derived(editSessions, ($s) => new Set($s.map((e) => e.remotePath)));

/** Sessions awaiting conflict resolution (drives the modal). */
export const editConflicts = derived(editSessions, ($s) => $s.filter((e) => e.hasConflict));

async function refresh() {
  editSessions.set(await api.listEditSessions());
}

let started = false;
let onSaved: ((name: string) => void) | null = null;
let onError: ((message: string) => void) | null = null;

export async function initEdit(opts?: {
  onSaved?: (name: string) => void;
  onError?: (message: string) => void;
}) {
  onSaved = opts?.onSaved ?? null;
  onError = opts?.onError ?? null;
  if (started) return;
  started = true;
  await refresh();
  await listen<EditSessionInfo>("edit://saved", (e) => {
    refresh();
    onSaved?.(e.payload.name);
  });
  await listen<EditSessionInfo>("edit://conflict", () => {
    refresh();
  });
  await listen<{ name: string; message: string }>("edit://error", (e) => {
    refresh();
    onError?.(`Couldn't save “${e.payload.name}”: ${e.payload.message}`);
  });
}

export async function closeSession(sessionId: number, keepTemp: boolean) {
  await api.closeEditSession(sessionId, keepTemp);
  await refresh();
}

export async function resolve(
  sessionId: number,
  action: "overwrite" | "saveAsCopy" | "discard",
) {
  await api.resolveConflict(sessionId, action);
  await refresh();
}
