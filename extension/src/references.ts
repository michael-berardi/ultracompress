import { createHash } from "node:crypto";

/** Session-local, bounded original-text cache. No paths, shell calls, or disk writes. */
export class UcReferences {
  private entries = new Map<string, { text: string; bytes: number }>();
  private bytes = 0;
  constructor(private maxBytes = 32 * 1024 * 1024, private maxEntries = 256) {}

  put(text: string): string | undefined {
    const bytes = Buffer.byteLength(text, "utf8");
    if (bytes > this.maxBytes || this.maxEntries < 1) return undefined;
    const ref = `uc:${createHash("sha256").update(text).digest("hex")}`;
    const previous = this.entries.get(ref);
    if (previous) {
      this.entries.delete(ref);
      this.bytes -= previous.bytes;
    }
    this.entries.set(ref, { text, bytes });
    this.bytes += bytes;
    while (this.bytes > this.maxBytes || this.entries.size > this.maxEntries) {
      const first = this.entries.keys().next().value!;
      this.bytes -= this.entries.get(first)!.bytes;
      this.entries.delete(first);
    }
    return ref;
  }

  get(ref: string): string | undefined {
    if (!/^uc:[a-f0-9]{64}$/.test(ref)) return undefined;
    return this.entries.get(ref)?.text;
  }

  clear(): void {
    this.entries.clear();
    this.bytes = 0;
  }
}
