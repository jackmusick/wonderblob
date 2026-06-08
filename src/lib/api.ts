import { invoke } from "@tauri-apps/api/core";

export type EntryKind = "file" | "dir" | "symlink";
export interface Entry {
  name: string;
  path: string;
  kind: EntryKind;
  size: number | null;
  modifiedMs: number | null;
}
export type StorageErrorKind =
  | "authFailed" | "notFound" | "permissionDenied" | "network"
  | "conflict" | "quotaExceeded" | "unsupported" | "other";
export interface StorageError { kind: StorageErrorKind; [k: string]: unknown }

export type AuthSpec =
  | { type: "agent" }
  | { type: "keyFile"; path: string; passphrase?: string }
  | { type: "password"; password: string };

export type AuthMethod =
  | { type: "agent" }
  | { type: "keyFile"; path: string }
  | { type: "password" };
export interface Bookmark {
  id: string;
  label: string;
  protocol: "sftp";
  host: string;
  port: number;
  username: string;
  authMethod: AuthMethod;
  initialPath: string | null;
}

export const api = {
  connectSftp: (args: { host: string; port: number; username: string; auth: AuthSpec }) =>
    invoke<number>("connect_sftp", { args }),
  disconnect: (id: number) => invoke<void>("disconnect", { id }),
  listDir: (id: number, path: string) => invoke<Entry[]>("list_dir", { id, path }),
  downloadFile: (id: number, remotePath: string, localPath: string) =>
    invoke<void>("download_file", { id, remotePath, localPath }),
  uploadFile: (id: number, localPath: string, remotePath: string) =>
    invoke<void>("upload_file", { id, localPath, remotePath }),
  deleteEntry: (id: number, path: string) => invoke<void>("delete_entry", { id, path }),
  renameEntry: (id: number, from: string, to: string) =>
    invoke<void>("rename_entry", { id, from, to }),
  makeDir: (id: number, path: string) => invoke<void>("make_dir", { id, path }),
  bookmarksList: () => invoke<Bookmark[]>("bookmarks_list"),
  bookmarkSave: (bookmark: Bookmark, secret?: string) =>
    invoke<void>("bookmark_save", { bookmark, secret }),
  bookmarkDelete: (id: string) => invoke<void>("bookmark_delete", { id }),
  connectBookmark: (id: string) => invoke<number>("connect_bookmark", { id }),
};
