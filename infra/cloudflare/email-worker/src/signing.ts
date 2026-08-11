const encoder = new TextEncoder();

export function canonicalEmailPayload(
  timestamp: number,
  nonce: string,
  bodySha256: string,
  envelopeFrom: string | undefined,
  envelopeTo: string,
): string {
  return [
    String(timestamp),
    nonce,
    bodySha256,
    envelopeFrom ?? "",
    envelopeTo,
  ].join("\n");
}

export async function sha256Hex(bytes: ArrayBuffer): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

export async function signEmailEvent(
  secret: string,
  timestamp: number,
  nonce: string,
  envelopeFrom: string | undefined,
  envelopeTo: string,
  rawMime: ArrayBuffer,
): Promise<string> {
  if (!secret) throw new Error("email ingest HMAC secret is not configured");
  const bodySha256 = await sha256Hex(rawMime);
  const payload = canonicalEmailPayload(
    timestamp,
    nonce,
    bodySha256,
    envelopeFrom,
    envelopeTo,
  );
  const key = await crypto.subtle.importKey(
    "raw",
    encoder.encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const signature = await crypto.subtle.sign("HMAC", key, encoder.encode(payload));
  let binary = "";
  for (const byte of new Uint8Array(signature)) binary += String.fromCharCode(byte);
  return btoa(binary);
}

export function randomNonce(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}
