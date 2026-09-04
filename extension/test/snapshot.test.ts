import { describe, expect, it } from "vitest";
import { buildSnap, listSnaps, snapFileName } from "../src/snapshot.ts";

const ENTRIES = [
  { type: "message", role: "user", text: "hello" },
  { type: "message", role: "assistant", text: "hi" },
];

describe("ultracompress snapshots (instant-snap compatible)", () => {
  it("builds a snap with metadata and full entries", () => {
    const { meta, payload } = buildSnap(ENTRIES, "threshold", 1_700_000_000_000);
    expect(meta.reason).toBe("threshold");
    expect(meta.entries).toBe(2);
    expect(meta.compactor).toBe("ultracompress");
    expect(meta.file).toMatch(/^snap-.*\.json$/);
    const parsed = JSON.parse(payload);
    expect(parsed.snap.createdAt).toBe(1_700_000_000_000);
    expect(parsed.entries).toHaveLength(2);
  });

  it("file names sort chronologically as strings", () => {
    const a = snapFileName(1_700_000_000_000);
    const b = snapFileName(1_700_000_001_000);
    expect(a < b).toBe(true);
  });
});
