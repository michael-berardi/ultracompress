/**
 * Provider payload adapters for snap frames.
 *
 * z.ai's coding endpoint is OpenAI-ish but expects image payloads as
 * `{type:"file", file:{file_data|file_url|file_id}}` parts instead of
 * `{type:"image_url", image_url:{url}}`. UltraCompress renders snap frames
 * as base64 PNG blocks; this adapter rewrites them per-provider so frames
 * reach models that pi's generic serializer can't service.
 */

export interface PayloadPart {
  type?: string;
  image_url?: { url?: string };
  file?: { file_data?: string; file_url?: string; file_id?: string };
  [k: string]: unknown;
}

export function isDataUrlImagePart(part: PayloadPart): boolean {
  return (
    part?.type === "image_url" &&
    typeof part.image_url?.url === "string" &&
    part.image_url.url.startsWith("data:")
  );
}

/** Rewrite one part into z.ai's expected shape. Returns null if not applicable. */
export function toZaiFilePart(part: PayloadPart): PayloadPart | null {
  if (!isDataUrlImagePart(part)) return null;
  return { type: "file", file: { file_data: part.image_url!.url } };
}

/** Detect whether a payload contains data-URL image parts. */
export function payloadHasDataUrlImages(payload: { messages?: Array<{ content?: unknown }> }): boolean {
  if (!Array.isArray(payload?.messages)) return false;
  for (const m of payload.messages) {
    if (!Array.isArray(m?.content)) continue;
    for (const part of m.content) {
      if (isDataUrlImagePart(part as PayloadPart)) return true;
    }
  }
  return false;
}

/** Convert all data-URL image parts in a payload to z.ai file parts, in place. */
export function adaptPayloadForZai(payload: { messages?: Array<{ content?: unknown }> }): number {
  let converted = 0;
  if (!Array.isArray(payload?.messages)) return 0;
  for (const m of payload.messages) {
    if (!Array.isArray(m?.content)) continue;
    const content = m.content as PayloadPart[];
    for (let i = 0; i < content.length; i++) {
      const converted_ = toZaiFilePart(content[i]);
      if (converted_) {
        content[i] = converted_;
        converted++;
      }
    }
  }
  return converted;
}

/** Error strings that indicate a provider rejected our image frames. */
export function isImageRejectionError(message: string): boolean {
  const m = (message ?? "").toLowerCase();
  return (
    (m.includes("image") || m.includes("file")) &&
    (m.includes("must contain") || m.includes("invalid") || m.includes("not supported") || m.includes("unexpected"))
  );
}
