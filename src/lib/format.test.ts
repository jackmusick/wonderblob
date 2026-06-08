import { describe, expect, it } from "vitest";
import { formatMtime, formatSize } from "./format";

describe("formatSize", () => {
  it("handles null, bytes, and scales", () => {
    expect(formatSize(null)).toBe("—");
    expect(formatSize(512)).toBe("512 B");
    expect(formatSize(1536)).toBe("1.5 KB");
    expect(formatSize(157286400)).toBe("150 MB");
  });

  it("handles boundaries and large values", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(1023)).toBe("1023 B");
    expect(formatSize(1024)).toBe("1.0 KB");
    expect(formatSize(1024 * 1024)).toBe("1.0 MB");
    expect(formatSize(5 * 1024 ** 4)).toBe("5.0 TB");
    // beyond TB clamps to TB
    expect(formatSize(2048 * 1024 ** 4)).toBe("2048 TB");
  });
});

describe("formatMtime", () => {
  it("returns em dash for null", () => {
    expect(formatMtime(null)).toBe("—");
  });

  it("formats a timestamp as a locale date+time", () => {
    const out = formatMtime(Date.UTC(2026, 0, 15, 12, 30));
    expect(out).toMatch(/2026/);
    expect(out.length).toBeGreaterThan(8);
  });
});
