import { describe, expect, it } from "vitest";
import { formatSpeed, percent } from "./transfer-format";

describe("transfer-format", () => {
  it("formatSpeed", () => {
    expect(formatSpeed(0)).toBe("");
    expect(formatSpeed(1536)).toBe("1.5 KB/s");
  });
  it("percent", () => {
    expect(percent(0, null)).toBe(-1);
    expect(percent(512, 1024)).toBe(50);
    expect(percent(2000, 1000)).toBe(100);
  });
});
