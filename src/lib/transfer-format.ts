import { formatSize } from "./format";

/** "1.4 MB/s"; 0 → "". */
export function formatSpeed(bytesPerSec: number): string {
  if (!bytesPerSec) return "";
  return `${formatSize(bytesPerSec)}/s`;
}

/** 0–100 integer; null total → indeterminate (-1). */
export function percent(transferred: number, total: number | null): number {
  if (total === null || total <= 0) return -1;
  return Math.min(100, Math.floor((transferred / total) * 100));
}
