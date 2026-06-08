import { describe, expect, it } from "vitest";
import { describeError, errorDetail } from "./errors";
import type { StorageError } from "./api";

function err(kind: StorageError["kind"], detail?: string): StorageError {
  return { kind, detail } as StorageError;
}

describe("describeError – list context", () => {
  it("authFailed", () => expect(describeError(err("authFailed"), "list")).toBe("Authentication failed."));
  it("network", () => expect(describeError(err("network"), "list")).toBe("Can't reach the server."));
  it("permissionDenied", () =>
    expect(describeError(err("permissionDenied"), "list")).toBe(
      "You don't have permission to view this folder."
    ));
  it("notFound", () => expect(describeError(err("notFound"), "list")).toBe("Folder not found."));
  it("other falls back to loading message", () =>
    expect(describeError(err("other"), "list")).toContain("Something went wrong loading this folder."));
});

describe("describeError – mutate context", () => {
  it("conflict", () =>
    expect(describeError(err("conflict"), "mutate")).toBe("Something with that name already exists."));
  it("quotaExceeded", () =>
    expect(describeError(err("quotaExceeded"), "mutate")).toBe("Operation failed: not enough space."));
  it("permissionDenied", () =>
    expect(describeError(err("permissionDenied"), "mutate")).toBe("Operation failed: permission denied."));
  it("unsupported", () =>
    expect(describeError(err("unsupported"), "mutate")).toBe(
      "Operation failed: not supported by this server."
    ));
  it("other falls back to generic mutate message", () =>
    expect(describeError(err("other"), "mutate")).toContain("Operation failed."));
});

describe("errorDetail", () => {
  it("reads the detail field", () =>
    expect(errorDetail({ kind: "other", detail: "boom" })).toBe("boom"));
  it("reads path when there is no detail (notFound)", () =>
    expect(errorDetail({ kind: "notFound", path: "/wbtest" })).toBe("/wbtest"));
  it("reads op for unsupported", () =>
    expect(errorDetail({ kind: "unsupported", op: "share_link" })).toBe("share_link"));
  it("never yields [object Object] for a fieldless error", () =>
    expect(errorDetail({ kind: "quotaExceeded" })).toBe(""));
  it("returns empty for null/undefined", () => {
    expect(errorDetail(null)).toBe("");
    expect(errorDetail(undefined)).toBe("");
  });
});
