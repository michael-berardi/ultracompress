import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import { MAX_SNAP_AGE_MS, MAX_SNAPS, MAX_SNAPS_BYTES, buildSnap, listSnaps, pruneSnaps, readSnapMeta, snapFileName, writeSnap, writeSnapEntries } from "../src/snapshot.ts";

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

  it("writes private snapshots and prunes the oldest", () => {
    const dir = mkdtempSync(join(tmpdir(), "ultracompress-privacy-"));
    try {
      const snaps = join(dir, ".steak-pi", "snaps");
      mkdirSync(join(dir, ".steak-pi"));
      mkdirSync(snaps);
      chmodSync(snaps, 0o777); // a pre-existing loose directory must be tightened
      const first = writeSnapEntries(dir, ENTRIES, "old", 1_700_000_000_000).file;
      const middle = buildSnap([], "middle", 1_700_000_001_000);
      const newest = buildSnap([], "new", 1_700_000_002_000);
      const middleFile = writeSnap(dir, middle.payload, middle.meta.file, middle.meta.createdAt);
      expect(statSync(snaps).mode & 0o777).toBe(0o700);
      expect(statSync(join(dir, ".steak-pi")).mode & 0o777).toBe(0o700);
      expect(statSync(join(snaps, first)).mode & 0o777).toBe(0o600);
      expect(statSync(middleFile).mode & 0o777).toBe(0o600);
      writeSnap(dir, newest.payload, newest.meta.file, newest.meta.createdAt);
      expect(listSnaps(dir).length).toBe(3);
      pruneSnaps(snaps, 2, 1024 * 1024, 1_700_000_002_500);
      const remaining = listSnaps(dir).map((m) => m.file).sort();
      expect(remaining).toHaveLength(2);
      expect(remaining).toContain(snapFileName(1_700_000_002_000));
      expect(remaining).not.toContain(first);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("prunes by size from the oldest end, never a newer snapshot first", () => {
    const dir = mkdtempSync(join(tmpdir(), "ultracompress-prune-"));
    try {
      const pad = "x".repeat(4000);
      const files = [0, 1, 2].map((i) => {
        const snap = buildSnap([{ type: "message", role: "user", text: pad }], `s${i}`, 1_700_000_000_000 + i * 1000);
        return writeSnap(dir, snap.payload, snap.meta.file, snap.meta.createdAt);
      });
      const each = statSync(files[2]).size;
      // Budget fits the two newest snapshots but not all three.
      pruneSnaps(join(dir, ".steak-pi", "snaps"), 20, each * 2 + 10, 1_700_000_002_500);
      const remaining = listSnaps(dir).map((m) => m.file).sort();
      expect(remaining).toEqual([snapFileName(1_700_000_001_000), snapFileName(1_700_000_002_000)].sort());
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("expires snapshots older than the retention age, keeping the newest one", () => {
    const dir = mkdtempSync(join(tmpdir(), "ultracompress-age-"));
    try {
      const now = Date.now();
      const newest = buildSnap([], "age-newest", now);
      writeSnap(dir, newest.payload, newest.meta.file, now);
      const snaps = join(dir, ".steak-pi", "snaps");
      const nowFile = newest.meta.file;
      const halfCreated = now - MAX_SNAP_AGE_MS / 2;
      const expiredCreated = now - MAX_SNAP_AGE_MS - 86_400_000;
      const halfFile = snapFileName(halfCreated);
      const expiredFile = snapFileName(expiredCreated);
      // Written directly so each fixture keeps the timestamp it was built with.
      for (const [file, createdAt] of [[halfFile, halfCreated], [expiredFile, expiredCreated]] as const) {
        writeFileSync(join(snaps, file), buildSnap([], file, createdAt).payload, { mode: 0o600 });
      }

      // At the oldest file's own clock nothing has expired yet.
      pruneSnaps(snaps, MAX_SNAPS, MAX_SNAPS_BYTES, expiredCreated);
      expect(listSnaps(dir).map((m) => m.file).sort()).toEqual([nowFile, halfFile, expiredFile].sort());

      // A month from now the aged snapshot goes; the half-aged one stays.
      pruneSnaps(snaps, MAX_SNAPS, MAX_SNAPS_BYTES, now);
      expect(listSnaps(dir).map((m) => m.file).sort()).toEqual([nowFile, halfFile].sort());

      // Once everything is past the limit the newest is still kept, so a
      // long-idle project keeps one restorable file rather than none.
      pruneSnaps(snaps, MAX_SNAPS, MAX_SNAPS_BYTES, now + MAX_SNAP_AGE_MS * 10);
      expect(listSnaps(dir).map((m) => m.file)).toEqual([nowFile]);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe("writeSnapEntries (streamed snapshots)", () => {
  const dir = mkdtempSync(join(tmpdir(), "ultracompress-snap-"));

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
