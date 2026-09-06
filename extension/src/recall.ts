import { resolve } from "node:path";

export const recallProperties = {
  query: { type: "string", minLength: 1, maxLength: 512, description: "Ranked keywords; use regex:true for an explicit regex." },
  scope: { type: "string", enum: ["lineage", "all"], description: "Default: current lineage. all = all branches of the selected session ONLY." },
  sessionFile: { type: "string", description: "Explicit other-session JSONL path. Omit to search only this session; never scans the session archive." },
  regex: { type: "boolean", description: "Interpret query as a regular expression. Default false." },
  role: { type: "string", enum: ["user", "assistant", "toolResult"], description: "Restrict message role before ranking." },
  toolName: { type: "string", description: "Restrict to results from this exact tool name." },
  afterEntry: { type: "string", description: "Search only after this entry ID in the selected scope (exclusive)." },
  beforeEntry: { type: "string", description: "Search only before this entry ID in the selected scope (exclusive)." },
  page: { type: "integer", minimum: 1, maximum: 1000000, description: "1-based page. Default 1." },
  perPage: { type: "integer", minimum: 1, maximum: 20, description: "Hits per page. Default 5." },
  snippetBytes: { type: "integer", minimum: 128, maximum: 4000, description: "UTF-8 bytes per excerpt, not tokens. Default 1000." },
  maxOutputBytes: { type: "integer", minimum: 1024, maximum: 32000, description: "Complete result JSON byte budget (transport wrapper excluded), not tokens. Default 12000." },
} as const;

export interface RecallSession {
  getSessionFile(): string | undefined;
  getLeafId(): string | null;
}

export function recallArgs(params: Record<string, unknown>, session: RecallSession): string[] {
  for (const key of Object.keys(params)) {
    if (!Object.hasOwn(recallProperties, key)) throw new Error(`Unknown recall option: ${key}`);
  }
  if (typeof params.query !== "string" || !params.query.trim() || Array.from(params.query).length > 512) {
    throw new Error("query must contain 1–512 characters");
  }
  const scope = params.scope === undefined ? "lineage" : params.scope;
  if (scope !== "lineage" && scope !== "all") throw new Error("scope must be lineage or all (within one session)");
  const currentFile = session.getSessionFile();
  const selectedFile = params.sessionFile === undefined ? currentFile : params.sessionFile;
  if (typeof selectedFile !== "string" || !selectedFile.trim()) throw new Error("No session file available; supply an explicit sessionFile");
  if (selectedFile.length > 4096) throw new Error("sessionFile is too long");
  const args = ["recall", "--session", selectedFile, "--query", params.query, "--scope", scope];
  // A resumed/tree-navigated session may have a different tip from the last
  // record on disk. Null is an empty branch, NOT permission to use another tip.
  if (scope === "lineage" && currentFile && resolve(selectedFile) === resolve(currentFile)) {
    args.push("--leaf", session.getLeafId() ?? "");
  }
  if (params.regex !== undefined && typeof params.regex !== "boolean") throw new Error("regex must be boolean");
  if (params.regex) args.push("--regex");
  for (const [key, flag] of [["role", "--role"], ["toolName", "--tool-name"], ["afterEntry", "--after-entry"], ["beforeEntry", "--before-entry"]]) {
    const value = params[key];
    if (value === undefined) continue;
    if (typeof value !== "string" || !value || value.length > 512) throw new Error(`${key} must contain 1–512 characters`);
    if (key === "role" && !["user", "assistant", "toolResult"].includes(value)) throw new Error("Invalid role");
    args.push(flag, value);
  }
  for (const [key, flag, min, max] of [
    ["page", "--page", 1, 1000000], ["perPage", "--per-page", 1, 20],
    ["snippetBytes", "--snippet-bytes", 128, 4000], ["maxOutputBytes", "--max-output-bytes", 1024, 32000],
  ] as const) {
    const value = params[key];
    if (value === undefined) continue;
    if (typeof value !== "number" || !Number.isInteger(value) || value < min || value > max) throw new Error(`${key} must be an integer from ${min} to ${max}`);
    args.push(flag, String(value));
  }
  return args;
}

// JSON form supports explicit paths containing spaces and the full tool API.
// Short form retains existing keyword + scope:all command compatibility.
export function parseRecallCommand(input: string): Record<string, unknown> {
  if (input.trim().startsWith("{")) {
    const params: unknown = JSON.parse(input);
    if (!params || typeof params !== "object" || Array.isArray(params)) throw new Error("Expected a recall options object");
    return params as Record<string, unknown>;
  }
  const params: Record<string, unknown> = {};
  const query = input.replace(/(?:^|\s)(scope|role|toolName|afterEntry|beforeEntry|page|perPage|snippetBytes|maxOutputBytes):(\S+)/g, (_match, key: string, value: string) => {
    if (params[key] !== undefined) throw new Error(`Duplicate recall option: ${key}`);
    params[key] = ["page", "perPage", "snippetBytes", "maxOutputBytes"].includes(key) ? Number(value) : value;
    return " ";
  }).trim();
  if (query.startsWith("/") && query.endsWith("/") && query.length > 2) {
    params.query = query.slice(1, -1); params.regex = true;
  } else params.query = query;
  return params;
}

export function recallText(data: unknown, budget = 12000): string {
  const text = JSON.stringify(data);
  if (!text || Buffer.byteLength(text, "utf8") > budget) throw new Error("Recall bridge exceeded the output byte budget; update the bridge or request smaller excerpts");
  return text;
}
