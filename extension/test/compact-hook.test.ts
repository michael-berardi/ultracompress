import { describe, expect, it } from "vitest";
import {
  buildCompactStdin,
  formatStatsLine,
  parseRcArgs,
  toCompactionResult,
  type CompactEventLike,
  type RcCompactResult,
} from "../src/compact-hook.ts";

describe("parseRcArgs", () => {
  it("parses keep and policy plus prompt", () => {
    const r = parseRcArgs("keep:3 policy:vcc check the deploy logs");
    expect(r.keep).toBe(3);
    expect(r.policy).toBe("vcc");
    expect(r.prompt).toBe("check the deploy logs");
  });

  it("handles bare prompt and empty args", () => {
    expect(parseRcArgs("just continue").keep).toBeNull();
    expect(parseRcArgs("just continue").prompt).toBe("just continue");
    expect(parseRcArgs(undefined)).toEqual({ keep: null, policy: null, prompt: "" });
  });

  it("keep:0 means compact everything", () => {
    expect(parseRcArgs("keep:0").keep).toBe(0);
  });

  it("parses /compact passthrough keep:N", () => {
    expect(parseRcArgs("keep:5").keep).toBe(5);
    expect(parseRcArgs("summarize auth work keep:2").keep).toBe(2);
  });
});

describe("buildCompactStdin", () => {
  const event: CompactEventLike = {
    preparation: { tokensBefore: 42_000, previousSummary: "[Session Goal]\n- old goal\n" },
    branchEntries: [
      { type: "session", id: "s0" },
      { type: "message", id: "e1", message: { role: "user", content: [{ type: "text", text: "hi" }] } },
      { type: "model_change", id: "mc1" },
      { type: "message", id: "e2", message: { role: "assistant", content: [{ type: "text", text: "hello" }] } },
    ],
    reason: "threshold",
  };

  it("keeps only message and compaction entries", () => {
    const stdin = buildCompactStdin(event, {
      policy: "auto", keepUserTurns: null, smartKeepTail: true, modelVision: true,
      ucBin: "uc", ucEnabled: true, ucMinChars: 1200, snapMinChars: 6000,
    });
    expect(stdin.entries.map((e) => e.type)).toEqual(["message", "message"]);
    expect(stdin.tokensBefore).toBe(42_000);
    expect(stdin.previousSummary).toContain("old goal");
    expect(stdin.vision).toBe("on");
  });

  it("vision auto when model unknown", () => {
    const stdin = buildCompactStdin(event, {
      policy: "auto", keepUserTurns: null, smartKeepTail: true, modelVision: null,
      ucBin: "uc", ucEnabled: true, ucMinChars: 1200, snapMinChars: 6000,
    });
    expect(stdin.vision).toBe("auto");
    expect(stdin.modelVision).toBeNull();
  });
});

const rcResult: RcCompactResult = {
  summary: "[Session Goal]\n- did things\n",
  first_kept_entry_id: "e7",
  details: { compactor: "rapid-compact", version: "0.1.0" },
  stats: {
    tokens_before_est: 50_000,
    tokens_after_est: 12_345,
    savings_pct: 75.3,
    summarized_messages: 40,
    kept_messages: 6,
    keep_user_turns_resolved: 3,
    smart_keep_adjusted: true,
    uc_blocks: 2,
    snap_blocks: 0,
    chars_per_token: 3.8,
    calibrated: true,
  },
  uc_status: { available: true, version: "uc 0.1.2" },
};

describe("toCompactionResult", () => {
  it("maps rc result into pi compaction shape", () => {
    const mapped = toCompactionResult(rcResult, 50_000);
    expect(mapped).not.toBeNull();
    expect(mapped!.summary).toContain("did things");
    expect(mapped!.firstKeptEntryId).toBe("e7");
    expect(mapped!.tokensBefore).toBe(50_000);
    expect(mapped!.details.compactor).toBe("rapid-compact");
    expect((mapped!.details.rcStats as { savings_pct: number }).savings_pct).toBeCloseTo(75.3);
  });

  it("rejects empty summaries", () => {
    expect(toCompactionResult({ ...rcResult, summary: "" }, undefined)).toBeNull();
  });
});

describe("formatStatsLine", () => {
  it("renders the toast line", () => {
    const line = formatStatsLine(rcResult);
    expect(line).toContain("50.0k → 12.3k tok (-75%)");
    expect(line).toContain("smart-keep → 3");
    expect(line).toContain("uc ×2");
  });
});
