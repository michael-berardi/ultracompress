import * as fs from "node:fs";
import * as path from "node:path";

/**
 * Pre-compaction snapshots — the safety net Instant Snap provided, now built
 * into Rapid Compact. Before every compaction the full session state is
 * serialized to .steak-pi/snaps/ so pre-compaction state is always
 * restorable. One local write at snap time; failure never blocks compaction.
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
    compactor: "rapid-compact",
  };
  return { meta, payload: JSON.stringify({ snap: meta, entries }, null, 0) };
}

export function writeSnap(cwd: string, payload: string, fileName: string): string {
  const dir = path.join(cwd, STATE_DIR, SNAPS_DIR);
  fs.mkdirSync(dir, { recursive: true });
  const file = path.join(dir, fileName);
  fs.writeFileSync(file, payload);
  return file;
}

export function listSnaps(cwd: string): SnapMeta[] {
  const dir = path.join(cwd, STATE_DIR, SNAPS_DIR);
  try {
    return fs
      .readdirSync(dir)
      .filter((f) => f.endsWith(".json"))
      .map((f) => {
        try {
          return (JSON.parse(fs.readFileSync(path.join(dir, f), "utf8"))?.snap ?? null) as SnapMeta | null;
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
