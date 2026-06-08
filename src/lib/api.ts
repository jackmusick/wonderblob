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

export type Protocol = "sftp" | "s3" | "azBlob";
export type AzAuthKind = "accountKey" | "connectionString" | "sas";

export interface S3Params {
  accessKeyId: string;
  region: string | null;
  endpoint: string | null;
  forcePathStyle: boolean;
}
export interface AzBlobParams {
  account: string;
  endpoint: string | null;
  authKind: AzAuthKind;
}

// NOTE(Task 8): host/port/username/authMethod are SFTP-only and now optional;
// cloud bookmarks carry `s3`/`azblob` instead. Task 9 finishes the frontend
// wiring (protocol picker, cloud bookmark forms, storing capabilities).
export interface Bookmark {
  id: string;
  label: string;
  protocol: Protocol;
  host?: string;
  port?: number;
  username?: string;
  authMethod?: AuthMethod;
  initialPath: string | null;
  s3?: S3Params | null;
  azblob?: AzBlobParams | null;
}

/** Capabilities the connected backend exposes; mirrors core `Capabilities`. */
export interface Capabilities {
  canPresign: boolean;
  canRename: boolean;
  canSetMtime: boolean;
}

/** Returned by every connect command (Task 8). */
export interface ConnectResult {
  id: number;
  capabilities: Capabilities;
}

export type TransferDirection = "up" | "down";
export type TransferStatus =
  | "queued" | "running" | "paused" | "completed" | "failed" | "canceled";

export interface Transfer {
  id: number;
  connectionId: number;
  direction: TransferDirection;
  remotePath: string;
  localPath: string;
  name: string;
  totalBytes: number | null;
  transferredBytes: number;
  status: TransferStatus;
  error: string | null;
  createdAtMs: number;
  updatedAtMs: number;
}

/** Payload of `transfer://progress`. */
export interface TransferProgress {
  id: number;
  transferredBytes: number;
  totalBytes: number | null;
  bytesPerSec: number;
}

export interface S3ConnectArgs {
  accessKeyId: string;
  secretAccessKey: string;
  region?: string | null;
  endpoint?: string | null;
  forcePathStyle?: boolean;
}
export interface AzBlobConnectArgs {
  account: string;
  endpoint?: string | null;
  authKind: AzAuthKind;
  secret: string;
}

export const api = {
  connectSftp: (args: { host: string; port: number; username: string; auth: AuthSpec }) =>
    invoke<ConnectResult>("connect_sftp", { args }),
  connectS3: (args: S3ConnectArgs) => invoke<ConnectResult>("connect_s3", { args }),
  connectAzblob: (args: AzBlobConnectArgs) => invoke<ConnectResult>("connect_azblob", { args }),
  shareLink: (id: number, path: string, expirySecs: number) =>
    invoke<string>("share_link", { id, path, expirySecs }),
  disconnect: (id: number) => invoke<void>("disconnect", { id }),
  listDir: (id: number, path: string) => invoke<Entry[]>("list_dir", { id, path }),
  enqueueDownload: (id: number, remotePath: string, localPath: string, totalBytes?: number) =>
    invoke<number>("enqueue_download", { id, remotePath, localPath, totalBytes: totalBytes ?? null }),
  enqueueUpload: (id: number, localPath: string, remotePath: string) =>
    invoke<number>("enqueue_upload", { id, localPath, remotePath }),
  pauseTransfer: (transferId: number) => invoke<void>("pause_transfer", { transferId }),
  resumeTransfer: (transferId: number, connectionId?: number) =>
    invoke<void>("resume_transfer", { transferId, connectionId: connectionId ?? null }),
  cancelTransfer: (transferId: number) => invoke<void>("cancel_transfer", { transferId }),
  listTransfers: () => invoke<Transfer[]>("list_transfers"),
  clearCompleted: () => invoke<number>("clear_completed"),
  deleteEntry: (id: number, path: string) => invoke<void>("delete_entry", { id, path }),
  renameEntry: (id: number, from: string, to: string) =>
    invoke<void>("rename_entry", { id, from, to }),
  makeDir: (id: number, path: string) => invoke<void>("make_dir", { id, path }),
  bookmarksList: () => invoke<Bookmark[]>("bookmarks_list"),
  bookmarkSave: (bookmark: Bookmark, secret?: string) =>
    invoke<void>("bookmark_save", { bookmark, secret }),
  bookmarkDelete: (id: string) => invoke<void>("bookmark_delete", { id }),
  connectBookmark: (id: string) => invoke<ConnectResult>("connect_bookmark", { id }),
};
