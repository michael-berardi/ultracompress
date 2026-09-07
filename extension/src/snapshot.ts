import * as fs from "node:fs";
import * as path from "node:path";

/**
 * Pre-compaction snapshots — the safety net Instant Snap provided, now built
 * into UltraCompress. Before every compaction the full session state is
 * serialized to .steak-pi/snaps/ so pre-compaction state is always
 * restorable. One local write at snap time; failure never blocks compaction.
 *
 * writeSnapEntries streams the JSON to disk entry by entry. Serializing the
 * whole session with one JSON.stringify call breaks once the payload crosses
 * V8's max string length (~512 MiB chars) with `RangeError: Invalid string
 * length`, which silently disabled the safety net on very large sessions.
 */

export const STATE_DIR = ".steak-pi";
export const SNAPS_DIR = "snaps";

export interface SnapMeta {
  file: string;
  reason: string;
  createdAt: number;
  entries: number;
  compactor: string;
}

export function snapFileName(nowMs: number): string {
  return `snap-${new Date(nowMs).toISOString().replace(/[:.]/g, "-")}.json`;
}

export function buildSnap(
  entries: unknown[],
  reason: string,
  nowMs: number,
): { meta: SnapMeta; payload: string } {
  const meta: SnapMeta = {
    file: snapFileName(nowMs),
    reason,
    createdAt: nowMs,
    entries: entries.length,
    compactor: "ultracompress",
  };
  return { meta, payload: JSON.stringify({ snap: meta, entries }, null, 0) };
}

/**
 * Stream a snapshot to disk without ever materializing the full JSON payload
 * in one string. Returns the same meta a buildSnap caller would have written.
 * A single entry that cannot be serialized on its own (beyond V8's max string
 * length) is replaced by an oversized-entry placeholder so the file stays
 * valid JSON; every other error removes the partial file and rethrows.
 */
export function writeSnapEntries(cwd: string, entries: unknown[], reason: string, nowMs: number): SnapMeta {
  const meta: SnapMeta = {
    file: snapFileName(nowMs),
    reason,
    createdAt: nowMs,
    entries: entries.length,
    compactor: "ultracompress",
  };
  const dir = path.join(cwd, STATE_DIR, SNAPS_DIR);
  fs.mkdirSync(dir, { recursive: true });
  const file = path.join(dir, meta.file);
  const fd = fs.openSync(file, "w");
  try {
    writeAll(fd, `{"snap":${JSON.stringify(meta)},"entries":[`);
    for (let i = 0; i < entries.length; i++) {
      if (i > 0) writeAll(fd, ",");
      writeAll(fd, stringifySnapEntry(entries[i], i));
    }
    writeAll(fd, "]}");
    fs.closeSync(fd);
    return meta;
  } catch (error) {
    try {
      fs.closeSync(fd);
    } catch {}
    try {
      fs.unlinkSync(file);
    } catch {}
    throw error;
  }
}

/** fs.writeSync may write fewer bytes than requested; loop until the whole chunk lands. */
function writeAll(fd: number, chunk: string): void {
  const buf = Buffer.from(chunk, "utf8");
  let offset = 0;
  while (offset < buf.length) {
    offset += fs.writeSync(fd, buf, offset, buf.length - offset);
  }
}

/**
 * Only V8's max-string-length failures become placeholders; a custom
 * RangeError thrown from user code (or a stack overflow) must surface so
 * the partial file is removed and the caller logs the real problem.
 */
function isMaxStringLengthError(error: unknown): boolean {
  return error instanceof RangeError && /invalid string length/i.test(error.message);
}

function stringifySnapEntry(entry: unknown, index: number): string {
  try {
    return JSON.stringify(entry) ?? "null";
  } catch (error) {
    if (isMaxStringLengthError(error)) {
      return JSON.stringify({ ucSnapOversizedEntry: true, index });
    }
    throw error;
  }
}

export function writeSnap(cwd: string, payload: string, fileName: string): string {
  const dir = path.join(cwd, STATE_DIR, SNAPS_DIR);
  fs.mkdirSync(dir, { recursive: true });
  const file = path.join(dir, fileName);
  fs.writeFileSync(file, payload);
  return file;
}

/** Head bytes inspected per snap file; meta always sits at the start of the file. */
const SNAP_META_HEAD_BYTES = 256 * 1024;

/**
 * Read the snap meta from the head of a snapshot file without loading the
 * whole payload. Snapshots streamed on oversized sessions exceed V8's max
 * string length, so a whole-file readFileSync/JSON.parse fails with
 * `RangeError: Invalid string length` and the meta would be lost.
 *
 * Constraints: the file must start with the exact compact prefix
 * `{"snap":` (the format this module always writes), and the meta object
 * must be fully contained in the inspected head bytes; anything else
 * yields null, matching the old malformed-file behavior.
 */
export function readSnapMeta(file: string): SnapMeta | null {
  const fd = fs.openSync(file, "r");
  try {
    const head = Buffer.alloc(SNAP_META_HEAD_BYTES);
    const bytes = fs.readSync(fd, head, 0, head.length, 0);
    const text = head.toString("utf8", 0, bytes);
    const prefix = "{\"snap\":";
    if (!text.startsWith(prefix)) return null;
    // Anchor on the meta object itself (the value of the "snap" key) and
    // escape-aware brace match from its opening brace.
    let start = prefix.length;
    while (start < text.length && text[start] !== "{") {
      if (text[start] !== " ") return null;
      start++;
    }
    if (start >= text.length) return null;
    let depth = 0;
    let inString = false;
    let escaped = false;
    let end = -1;
    for (let i = start; i < text.length; i++) {
      const ch = text[i];
      if (inString) {
        if (escaped) escaped = false;
        else if (ch === "\\") escaped = true;
        else if (ch === '"') inString = false;
        continue;
      }
      if (ch === '"') inString = true;
      else if (ch === "{") depth++;
      else if (ch === "}") {
        depth--;
        if (depth === 0) {
          end = i + 1;
          break;
        }
      }
    }
    if (end === -1) return null;
    const meta = JSON.parse(text.slice(start, end)) as Partial<SnapMeta> | null;
    if (!meta || typeof meta !== "object") return null;
    if (typeof meta.file !== "string" || typeof meta.createdAt !== "number" || meta.entries === undefined) {
      return null;
    }
    return meta as SnapMeta;
  } catch {
    return null;
  } finally {
    fs.closeSync(fd);
  }
}

export function listSnaps(cwd: string): SnapMeta[] {
  const dir = path.join(cwd, STATE_DIR, SNAPS_DIR);
  try {
    return fs
      .readdirSync(dir)
      .filter((f) => f.endsWith(".json"))
      .map((f) => {
        try {
          return readSnapMeta(path.join(dir, f));
        } catch {
          return null;
        }
      })
      .filter((m): m is SnapMeta => Boolean(m))
      .sort((a, b) => b.createdAt - a.createdAt);
  } catch {
    return [];
  }
}
