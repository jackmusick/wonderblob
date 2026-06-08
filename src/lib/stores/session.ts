import { writable } from "svelte/store";
import type { Bookmark } from "../api";

export const activeConnection = writable<{ id: number; bookmark: Bookmark } | null>(null);
export const currentPath = writable<string>("/");
