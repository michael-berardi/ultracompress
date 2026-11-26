import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { DEFAULT_SNAPSHOT_RETENTION, normalizeSnapshotRetention, type SnapshotRetention } from "./snapshot";

export interface SnapSettings {
  enabled: boolean;
  minChars: number;
  placement: "nextUser" | "inline";
  imageTokensPerFrame: number | null;
  /** Providers whose wire format is proven for inline base64 images.
   *  Frames are only offered to these; everyone else keeps raw text + VCC. */
  providers: string[];
}

export interface UltraCompressSettings {
  policy: "auto" | "vcc" | "snap";
  overrideDefaultCompaction: boolean;
  smartKeepTail: boolean;
  keepUserTurns: number | null;
  ultracompressBin: string;
  snap: SnapSettings;
  snapshot: { enabled: boolean } & SnapshotRetention;
  /** Byte ceiling for automatically generated summaries; explicit compaction is never capped. */
  summaryMaxBytes: number;
  /** Footer update notice; the CLI's own config still owns install opt-in. */
  checkForUpdates: boolean;
  debug: boolean;
}

export const DEFAULT_SETTINGS: UltraCompressSettings = {
  policy: "auto",
  overrideDefaultCompaction: true,
  smartKeepTail: true,
  keepUserTurns: null,
  ultracompressBin: "",
  snap: { enabled: true, minChars: 6000, placement: "nextUser", imageTokensPerFrame: null, providers: ["anthropic", "google"] },
  snapshot: { enabled: true, ...DEFAULT_SNAPSHOT_RETENTION },
  summaryMaxBytes: 16384,
  checkForUpdates: true,
  debug: false,
};

export const SETTINGS_PATH = path.join(os.homedir(), ".pi", "agent", "ultracompress.json");
export const LEGACY_SETTINGS_PATH = path.join(os.homedir(), ".pi", "agent", "rapid-compact.json");

/** Merge partial user JSON over defaults; unknown keys ignored, types coerced. */
export function mergeSettings(raw: unknown): UltraCompressSettings {
  const out: UltraCompressSettings = structuredClone(DEFAULT_SETTINGS);
  if (raw === null || typeof raw !== "object") return out;
  const obj = raw as Record<string, unknown>;
  if (isPolicy(obj.policy)) out.policy = obj.policy;
  if (typeof obj.overrideDefaultCompaction === "boolean") out.overrideDefaultCompaction = obj.overrideDefaultCompaction;
  if (typeof obj.smartKeepTail === "boolean") out.smartKeepTail = obj.smartKeepTail;
  if (typeof obj.keepUserTurns === "number" && Number.isFinite(obj.keepUserTurns)) {
    out.keepUserTurns = Math.max(0, Math.floor(obj.keepUserTurns));
  } else if (obj.keepUserTurns === null) {
    out.keepUserTurns = null;
  }
  if (typeof obj.ultracompressBin === "string") out.ultracompressBin = obj.ultracompressBin;
  // One-release migration alias; new files always write ultracompressBin.
  else if (typeof obj.rcBin === "string") out.ultracompressBin = obj.rcBin;
  if (typeof obj.checkForUpdates === "boolean") out.checkForUpdates = obj.checkForUpdates;
  if (typeof obj.debug === "boolean") out.debug = obj.debug;
  if (typeof obj.summaryMaxBytes === "number" && Number.isFinite(obj.summaryMaxBytes)) {
    out.summaryMaxBytes = Math.max(1024, Math.min(65536, Math.floor(obj.summaryMaxBytes)));
  }
  if (obj.snap && typeof obj.snap === "object") {
    const snap = obj.snap as Record<string, unknown>;
    if (typeof snap.enabled === "boolean") out.snap.enabled = snap.enabled;
    if (typeof snap.minChars === "number") out.snap.minChars = Math.max(0, snap.minChars);
    if (snap.placement === "nextUser" || snap.placement === "inline") out.snap.placement = snap.placement;
    if (typeof snap.imageTokensPerFrame === "number") out.snap.imageTokensPerFrame = snap.imageTokensPerFrame;
    else if (snap.imageTokensPerFrame === null) out.snap.imageTokensPerFrame = null;
    if (Array.isArray(snap.providers)) {
      out.snap.providers = snap.providers.filter((x) => typeof x === "string");
    }
  }
  if (obj.snapshot && typeof obj.snapshot === "object") {
    const sn = obj.snapshot as Record<string, unknown>;
    if (typeof sn.enabled === "boolean") out.snapshot.enabled = sn.enabled;
    Object.assign(out.snapshot, normalizeSnapshotRetention(sn));
  }
  return out;
}

function isPolicy(v: unknown): v is UltraCompressSettings["policy"] {
  return v === "auto" || v === "vcc" || v === "snap";
}

/** Config can name binaries and paths; keep it owner-only. */
function writeSettingsFile(settingsPath: string, settings: UltraCompressSettings): void {
  fs.mkdirSync(path.dirname(settingsPath), { recursive: true, mode: 0o700 });
  try {
    fs.chmodSync(path.dirname(settingsPath), 0o700);
  } catch {}
  fs.writeFileSync(settingsPath, JSON.stringify(settings, null, 2) + "\n", { mode: 0o600 });
  try {
    fs.chmodSync(settingsPath, 0o600);
  } catch {}
}

/** Load settings, scaffolding the file with defaults on first run. */
export function loadSettings(settingsPath = SETTINGS_PATH): UltraCompressSettings {
  try {
    const raw = fs.readFileSync(settingsPath, "utf8");
    return mergeSettings(JSON.parse(raw));
  } catch {
    // Migrate the old product-name config once; never delete user data.
    if (settingsPath === SETTINGS_PATH) {
      try {
        const migrated = mergeSettings(JSON.parse(fs.readFileSync(LEGACY_SETTINGS_PATH, "utf8")));
        writeSettingsFile(settingsPath, migrated);
        return migrated;
      } catch {
        // no legacy config — scaffold defaults below
      }
    }
    try {
      writeSettingsFile(settingsPath, DEFAULT_SETTINGS);
    } catch {
      // read-only environments still work with in-memory defaults
    }
    return structuredClone(DEFAULT_SETTINGS);
  }
}

/** Directory of a module URL; percent-decoded so paths with spaces resolve. */
export function moduleDir(url: string): string {
  return path.dirname(fileURLToPath(url));
}

/** Extension package version, read from package.json so reporting never drifts. */
export function packageVersion(url: string = import.meta.url): string {
  try {
    const pkg = JSON.parse(fs.readFileSync(path.join(moduleDir(url), "..", "package.json"), "utf8")) as { version?: unknown };
    return typeof pkg.version === "string" && pkg.version ? pkg.version : "unknown";
  } catch {
    return "unknown";
  }
}

/** Resolve the UltraCompress binary: env → config → UltraTerm-managed → local → dev → PATH. */
export function resolveUltraCompressBin(settings: UltraCompressSettings, extensionDir = moduleDir(import.meta.url)): string {
  const candidates: string[] = [];
  if (process.env.ULTRACOMPRESS_BIN) candidates.push(process.env.ULTRACOMPRESS_BIN);
  if (settings.ultracompressBin) {
    candidates.push(settings.ultracompressBin.replace(/^~(?=\/|$)/, os.homedir()));
  }
  candidates.push(path.join(os.homedir(), ".ultraterm", "bin", "ultracompress"));
  candidates.push(path.join(os.homedir(), ".local", "bin", "ultracompress"));
  candidates.push(path.join(os.homedir(), ".cargo", "bin", "ultracompress"));
  // src/ → extension/ → repo root
  candidates.push(path.join(extensionDir, "..", "..", "target", "release", "ultracompress"));
  candidates.push(path.join(extensionDir, "target", "release", "ultracompress"));
  // Compatibility with installs from before the product rename.
  if (process.env.RC_BIN) candidates.push(process.env.RC_BIN);
  candidates.push(path.join(os.homedir(), ".local", "bin", "rc"));
  candidates.push(path.join(os.homedir(), ".cargo", "bin", "rc"));
  for (const c of candidates) {
    try {
      fs.accessSync(c, fs.constants.X_OK);
      return c;
    } catch {
      // try next
    }
  }
  return "ultracompress"; // final fallback: PATH
}

/** Whether `bin` resolves to an executable file, directly or through PATH. */
export function isUltraCompressBinAvailable(bin: string, env: NodeJS.ProcessEnv = process.env): boolean {
  const executable = (candidate: string) => {
    try {
      fs.accessSync(candidate, fs.constants.X_OK);
      return fs.statSync(candidate).isFile();
    } catch {
      return false;
    }
  };
  if (bin.includes(path.sep)) return executable(bin);
  const exts = process.platform === "win32" ? (env.PATHEXT ?? ".EXE;.CMD;.BAT").split(";") : [""];
  return (env.PATH ?? "")
    .split(path.delimiter)
    .filter(Boolean)
    .some((dir) => exts.some((ext) => executable(path.join(dir, bin + ext))));
}
