import { browser } from "$app/environment";
import { writable } from "svelte/store";

export type Theme = "system" | "light" | "dark";

export interface Prefs {
  /** Force a color scheme, or follow the OS ("system"). */
  theme: Theme;
  /** When false, the file-list Delete acts immediately (no two-press confirm). */
  confirmDelete: boolean;
  /** Which optional file-list columns are shown. Name is always shown. */
  columns: { size: boolean; modified: boolean };
  /** Pixel widths of the optional columns (drag-resizable in the header). */
  colWidths: { size: number; modified: number };
}

export const DEFAULT_PREFS: Prefs = {
  theme: "system",
  confirmDelete: true,
  columns: { size: true, modified: true },
  colWidths: { size: 90, modified: 160 },
};

const KEY = "wb:prefs";

function load(): Prefs {
  if (!browser) return DEFAULT_PREFS;
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return DEFAULT_PREFS;
    const saved = JSON.parse(raw);
    // Shallow-merge so new pref fields pick up their defaults on upgrade.
    return {
      ...DEFAULT_PREFS,
      ...saved,
      columns: { ...DEFAULT_PREFS.columns, ...saved.columns },
      colWidths: { ...DEFAULT_PREFS.colWidths, ...saved.colWidths },
    };
  } catch {
    return DEFAULT_PREFS;
  }
}

export const prefs = writable<Prefs>(load());

if (browser) {
  prefs.subscribe((p) => {
    try {
      localStorage.setItem(KEY, JSON.stringify(p));
    } catch {
      /* storage full / unavailable — non-fatal */
    }
  });
}

/** Apply the theme preference to the document root (drives tokens.css). */
export function applyTheme(theme: Theme) {
  if (!browser) return;
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
}
