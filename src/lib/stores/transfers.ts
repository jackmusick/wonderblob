import { derived, writable } from "svelte/store";
import { listen } from "@tauri-apps/api/event";
import { api, type Transfer, type TransferProgress } from "$lib/api";

/** id → Transfer, plus a transient bytesPerSec keyed alongside. */
export const transfers = writable<Map<number, Transfer>>(new Map());
export const transferSpeed = writable<Map<number, number>>(new Map());

/** Newest-first array for rendering. */
export const transferList = derived(transfers, ($t) =>
  [...$t.values()].sort((a, b) => b.createdAtMs - a.createdAtMs)
);

/** Count of active (queued/running/paused) transfers for the toolbar badge. */
export const activeTransferCount = derived(transferList, ($l) =>
  $l.filter(
    (t) => t.status === "queued" || t.status === "running" || t.status === "paused"
  ).length
);

let started = false;

export async function initTransfers() {
  if (started) return;
  started = true;

  const initial = await api.listTransfers();
  transfers.set(new Map(initial.map((t) => [t.id, t])));

  await listen<Transfer>("transfer://state", (e) => {
    transfers.update((m) => {
      const next = new Map(m);
      next.set(e.payload.id, e.payload);
      return next;
    });
  });

  await listen<TransferProgress>("transfer://progress", (e) => {
    const p = e.payload;
    transfers.update((m) => {
      const cur = m.get(p.id);
      if (!cur) return m;
      const next = new Map(m);
      next.set(p.id, {
        ...cur,
        transferredBytes: p.transferredBytes,
        totalBytes: p.totalBytes ?? cur.totalBytes,
      });
      return next;
    });
    transferSpeed.update((s) => {
      const next = new Map(s);
      next.set(p.id, p.bytesPerSec);
      return next;
    });
  });
}

export async function clearCompleted() {
  await api.clearCompleted();
  transfers.update((m) => {
    const next = new Map(m);
    for (const [id, t] of next) if (t.status === "completed") next.delete(id);
    return next;
  });
}
