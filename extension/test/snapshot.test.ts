import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import { buildSnap, listSnaps, readSnapMeta, snapFileName, writeSnapEntries } from "../src/snapshot.ts";

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

describe("writeSnapEntries (streamed snapshots)", () => {
  const dir = mkdtempSync(join(tmpdir(), "uc-snap-"));

  afterAll(() => {
    try {
      rmSync(dir, { recursive: true, force: true });
    } catch {}
  });

  it("writes output identical to the historical single-string payload", () => {
    const meta = writeSnapEntries(dir, ENTRIES, "threshold", 1_700_000_000_000);
    const onDisk = readFileSync(join(dir, ".steak-pi", "snaps", meta.file), "utf8");
    const { payload } = buildSnap(ENTRIES, "threshold", 1_700_000_000_000);
    expect(onDisk).toBe(payload);
    expect(listSnaps(dir)[0]?.file).toBe(meta.file);
  });

  it("round-trips empty, unicode, and undefined-element entries", () => {
    const entries = [[], [{ text: "α β 日本語 café \"quoted\"" }], [undefined, null, 3, "x"]];
    const meta = writeSnapEntries(dir, entries, "test", 1_700_000_000_001);
    const onDisk = readFileSync(join(dir, ".steak-pi", "snaps", meta.file), "utf8");
    expect(onDisk).toBe(JSON.stringify({ snap: meta, entries }));
    expect(JSON.parse(onDisk).entries).toHaveLength(3);
  });

  it("persists thousands of entries without accumulating one giant string", () => {
    const entries = Array.from({ length: 5_000 }, (_, i) => ({
      type: "message",
      id: `entry-${i}`,
      message: { role: "toolResult", content: [{ type: "text", text: "z".repeat(2_000) }] },
    }));
    const meta = writeSnapEntries(dir, entries, "bulk", 1_700_000_000_002);
    const file = join(dir, ".steak-pi", "snaps", meta.file);
    const onDisk = readFileSync(file, "utf8");
    expect(onDisk.endsWith("]}")).toBe(true);
    expect(onDisk.startsWith("{\"snap\":")).toBe(true);
    const parsed = JSON.parse(onDisk);
    expect(parsed.entries).toHaveLength(5_000);
    expect(parsed.entries[4_999].id).toBe("entry-4999");
  }, 30_000);

  it("keeps the file valid JSON when a single entry exceeds V8 max string length", () => {
    // Allocatable string (below the char limit) whose serialized wrapper lands above it.
    const oversized = "y".repeat(536_870_876);
    const entries = [{ ok: true }, { tooBig: oversized }];
    expect(oversized.length).toBeLessThan(536_870_888);
    expect(() => JSON.stringify({ tooBig: oversized })).toThrow(RangeError);
    const meta = writeSnapEntries(dir, entries, "oversize", 1_700_000_000_003);
    const file = join(dir, ".steak-pi", "snaps", meta.file);
    const onDisk = readFileSync(file, "utf8");
    expect(onDisk.startsWith("{\"snap\":")).toBe(true);
    expect(onDisk.endsWith("]}")).toBe(true);
    expect(onDisk).toContain("ucSnapOversizedEntry");
    expect(onDisk.length).toBeLessThan(1_000);
    expect(meta.entries).toBe(2);
  }, 60_000);

  it("removes the partial file when an entry fails to serialize", () => {
    const badEntries = [
      { ok: true },
      {
        get toJSON() {
          throw new TypeError("boom");
        },
      },
    ];
    expect(() => writeSnapEntries(dir, badEntries, "boom", 1_700_000_000_005)).toThrow(TypeError);
    const files = listSnaps(dir).map((m) => m.file);
    expect(files).not.toContain(snapFileName(1_700_000_000_005));
  });

  it("does not mask a custom RangeError as an oversized entry", () => {
    const badEntries = [
      { ok: true },
      {
        get toJSON() {
          throw new RangeError("custom domain failure");
        },
      },
    ];
    expect(() => writeSnapEntries(dir, badEntries, "custom-range", 1_700_000_000_007)).toThrow(/custom domain failure/);
    const files = listSnaps(dir).map((m) => m.file);
    expect(files).not.toContain(snapFileName(1_700_000_000_007));
  });

  it("reads meta from the head without loading oversized snap payloads", () => {
    const snapDir = join(dir, ".steak-pi", "snaps");
    const meta = { file: "snap-big.json", reason: "threshold", createdAt: 1_700_000_000_006, entries: 9_000, compactor: "ultracompress" };
    // A streamed snapshot of a huge session is far beyond V8 max string length.
    // Simulate the readable head plus hundreds of megabytes that follow.
    const head = `{"snap":${JSON.stringify(meta)},"entries":[`;
    const file = join(snapDir, "snap-big.json");
    const fd = require("node:fs").openSync(file, "w");
    try {
      require("node:fs").writeSync(fd, head);
      const chunk = "y".repeat(1024 * 1024);
      for (let i = 0; i < 8; i++) require("node:fs").writeSync(fd, chunk); // representative tail bulk
    } finally {
      require("node:fs").closeSync(fd);
    }
    expect(readSnapMeta(file)).toEqual(meta);
    expect(listSnaps(dir).some((m) => m.file === "snap-big.json")).toBe(true);
  });

  it("returns null for malformed or foreign snap files", () => {
    const snapDir = join(dir, ".steak-pi", "snaps");
    const garbage = join(snapDir, "snap-garbage.json");
    writeFileSync(garbage, "{not json");
    expect(readSnapMeta(garbage)).toBeNull();
    const foreign = join(snapDir, "snap-foreign.json");
    writeFileSync(foreign, JSON.stringify({ something: "else without snap" }));
    expect(readSnapMeta(foreign)).toBeNull();
    const truncatedMeta = join(snapDir, "snap-truncated.json");
    writeFileSync(truncatedMeta, '{"snap":{"file":"x.json"');
    expect(readSnapMeta(truncatedMeta)).toBeNull();
  });
});
