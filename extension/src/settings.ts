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

export interface RcSettings {
  policy: "auto" | "vcc" | "snap" | "uc";
  overrideDefaultCompaction: boolean;
  smartKeepTail: boolean;
  keepUserTurns: number | null;
  rcBin: string;
  uc: UcSettings;
  snap: SnapSettings;
  snapshot: { enabled: boolean };
  debug: boolean;
}

export const DEFAULT_SETTINGS: RcSettings = {
  policy: "auto",
  overrideDefaultCompaction: true,
  smartKeepTail: true,
  keepUserTurns: null,
  rcBin: "",
  uc: { enabled: true, bin: "uc", minChars: 1200 },
  snap: { enabled: true, minChars: 6000, placement: "nextUser", imageTokensPerFrame: null, providers: ["anthropic", "google"] },
  snapshot: { enabled: true },
  debug: false,
};

export const SETTINGS_PATH = path.join(os.homedir(), ".pi", "agent", "rapid-compact.json");

/** Merge partial user JSON over defaults; unknown keys ignored, types coerced. */
export function mergeSettings(raw: unknown): RcSettings {
  const out: RcSettings = structuredClone(DEFAULT_SETTINGS);
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
  if (typeof obj.rcBin === "string") out.rcBin = obj.rcBin;
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

function isPolicy(v: unknown): v is RcSettings["policy"] {
  return v === "auto" || v === "vcc" || v === "snap" || v === "uc";
}

/** Load settings, scaffolding the file with defaults on first run. */
export function loadSettings(settingsPath = SETTINGS_PATH): RcSettings {
  try {
    const raw = fs.readFileSync(settingsPath, "utf8");
    return mergeSettings(JSON.parse(raw));
  } catch {
    try {
      fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
      fs.writeFileSync(settingsPath, JSON.stringify(DEFAULT_SETTINGS, null, 2) + "\n");
    } catch {
      // read-only environments still work with in-memory defaults
    }
    return structuredClone(DEFAULT_SETTINGS);
  }
}

/** Resolve the rc binary: env → config → ~/.local/bin/rc → ~/.cargo/bin/rc → dev build → PATH. */
export function resolveRcBin(settings: RcSettings, extensionDir = path.dirname(new URL(import.meta.url).pathname)): string {
  const candidates: string[] = [];
  if (process.env.RC_BIN) candidates.push(process.env.RC_BIN);
  if (settings.rcBin) {
    candidates.push(settings.rcBin.replace(/^~(?=\/|$)/, os.homedir()));
  }
  candidates.push(path.join(os.homedir(), ".local", "bin", "rc"));
  candidates.push(path.join(os.homedir(), ".cargo", "bin", "rc"));
  // src/ → extension/ → repo root
  candidates.push(path.join(extensionDir, "..", "..", "target", "release", "rc"));
  candidates.push(path.join(extensionDir, "target", "release", "rc"));
  for (const c of candidates) {
    try {
      fs.accessSync(c, fs.constants.X_OK);
      return c;
    } catch {
      // try next
    }
  }
  return "rc"; // final fallback: PATH
}
