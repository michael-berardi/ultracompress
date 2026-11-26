/** Bounded exact-text memoization; safe across deep-copied/mutated messages. */
export class TextKeyCache {
  private entries = new Map<string, string>();
  private chars = 0;
  private scope = "";

  constructor(private maxChars = 4 * 1024 * 1024, private maxEntries = 256) {}

  setScope(scope: string): void {
    if (scope !== this.scope) {
      this.clear();
      this.scope = scope;
    }
  }

  get(text: string, compute: () => string): string {
    const known = this.entries.get(text);
    if (known !== undefined) {
      this.entries.delete(text);
      this.entries.set(text, known);
      return known;
    }
    const key = compute();
    if (text.length > this.maxChars || this.maxEntries < 1) return key;
    this.entries.set(text, key);
    this.chars += text.length;
    while (this.chars > this.maxChars || this.entries.size > this.maxEntries) {
      const first = this.entries.keys().next().value!;
      this.chars -= first.length;
      this.entries.delete(first);
    }
    return key;
  }

  clear(): void {
    this.entries.clear();
    this.chars = 0;
  }
}
