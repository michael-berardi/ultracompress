import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

export interface UcSettings {
  enabled: boolean;
  bin: string;
  minChars: number;
}

export interface SnapSettings {
  enabled: boolean;
  minChars: number;
  placement: "nextUser" | "inline";
  imageTokensPerFrame: number | null;
  /** Providers whose wire format is proven for inline base64 images.
   *  Frames are only offered to these; everyone else gets UC + VCC. */
  providers: string[];
}

export interface UltraCompressSettings {
  policy: "auto" | "vcc" | "snap" | "uc";
  overrideDefaultCompaction: boolean;
  smartKeepTail: boolean;
  keepUserTurns: number | null;
  ultracompressBin: string;
  uc: UcSettings;
  snap: SnapSettings;
  snapshot: { enabled: boolean };
  debug: boolean;
}

export const DEFAULT_SETTINGS: UltraCompressSettings = {
  policy: "auto",
  overrideDefaultCompaction: true,
  smartKeepTail: true,
  keepUserTurns: null,
  ultracompressBin: "",
  uc: { enabled: true, bin: "uc", minChars: 1200 },
  snap: { enabled: true, minChars: 6000, placement: "nextUser", imageTokensPerFrame: null, providers: ["anthropic", "google"] },
  snapshot: { enabled: true },
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
  if (typeof obj.debug === "boolean") out.debug = obj.debug;
  if (obj.uc && typeof obj.uc === "object") {
    const uc = obj.uc as Record<string, unknown>;
    if (typeof uc.enabled === "boolean") out.uc.enabled = uc.enabled;
    if (typeof uc.bin === "string") out.uc.bin = uc.bin;
    if (typeof uc.minChars === "number") out.uc.minChars = Math.max(0, uc.minChars);
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
  }
  return out;
}

function isPolicy(v: unknown): v is UltraCompressSettings["policy"] {
  return v === "auto" || v === "vcc" || v === "snap" || v === "uc";
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
        fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
        fs.writeFileSync(settingsPath, JSON.stringify(migrated, null, 2) + "\n");
        return migrated;
      } catch {
        // no legacy config — scaffold defaults below
      }
    }
    try {
      fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
      fs.writeFileSync(settingsPath, JSON.stringify(DEFAULT_SETTINGS, null, 2) + "\n");
    } catch {
      // read-only environments still work with in-memory defaults
    }
    return structuredClone(DEFAULT_SETTINGS);
  }
}

/** Resolve the UltraCompress binary: env → config → UltraTerm-managed → local → dev → PATH. */
export function resolveUltraCompressBin(settings: UltraCompressSettings, extensionDir = path.dirname(new URL(import.meta.url).pathname)): string {
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
