import type { StorageError } from "./api";

export type ErrorContext = "list" | "mutate";

/**
 * Maps a StorageError to a user-facing string.
 *   ctx "list"   – used when loading a directory listing fails (full-pane error state)
 *   ctx "mutate" – used for rename / delete / mkdir / upload failures (toast/strip)
 */
export function describeError(e: unknown, ctx: ErrorContext): string {
  const err = e as StorageError;
  switch (err?.kind) {
    case "authFailed":
      return ctx === "list"
        ? "Authentication failed."
        : "Operation failed: authentication error.";
    case "network":
      return ctx === "list"
        ? "Can't reach the server."
        : "Operation failed: can't reach the server.";
    case "permissionDenied":
      return ctx === "list"
        ? "You don't have permission to view this folder."
        : "Operation failed: permission denied.";
    case "notFound":
      return ctx === "list"
        ? "Folder not found."
        : "Operation failed: item not found.";
    case "conflict":
      return "Something with that name already exists.";
    case "quotaExceeded":
      return "Operation failed: not enough space.";
    case "unsupported":
      return "Operation failed: not supported by this server.";
    default: {
      const raw = e as Record<string, unknown>;
      const detail = typeof raw?.detail === "string" ? raw.detail : String(e);
      return ctx === "list"
        ? `Something went wrong loading this folder.${detail ? " " + detail : ""}`
        : `Operation failed.${detail ? " " + detail : ""}`;
    }
  }
}
