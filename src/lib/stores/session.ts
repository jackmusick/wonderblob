import { writable } from "svelte/store";
import type { Bookmark, Capabilities } from "../api";

export const activeConnection = writable<{
  id: number;
  bookmark: Bookmark;
  capabilities: Capabilities;
} | null>(null);
export const currentPath = writable<string>("/");
