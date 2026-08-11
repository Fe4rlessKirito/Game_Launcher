import { randomNonce, signEmailEvent } from "./signing";

export interface Env {
  PROVISIONING_INGEST_URL: string;
  INGEST_HMAC_SECRET: string;
  DEBUG_FORWARD_ENABLED?: string;
  DEBUG_FORWARD_ADDRESS?: string;
}

interface EmailMessage {
  from: string;
  to: string;
  raw: ReadableStream<Uint8Array>;
  setReject(reason: string): void;
  forward(address: string): Promise<void>;
}

interface ExecutionContext {
  waitUntil(promise: Promise<unknown>): void;
}

function enabled(value: string | undefined): boolean {
  return ["1", "true", "yes", "on"].includes((value ?? "").trim().toLowerCase());
}

export default {
  async email(message: EmailMessage, env: Env, ctx: ExecutionContext): Promise<void> {
    if (!env.PROVISIONING_INGEST_URL || !env.INGEST_HMAC_SECRET) {
      message.setReject("provisioning email ingest is not configured");
      return;
    }
    const rawMime = await new Response(message.raw).arrayBuffer();
    const timestamp = Math.floor(Date.now() / 1000);
    const nonce = randomNonce();
    const envelopeFrom = message.from.trim() || undefined;
    const envelopeTo = message.to.trim().toLowerCase();
    const signature = await signEmailEvent(
      env.INGEST_HMAC_SECRET,
      timestamp,
      nonce,
      envelopeFrom,
      envelopeTo,
      rawMime,
    );
    const response = await fetch(env.PROVISIONING_INGEST_URL, {
      method: "POST",
      headers: {
        "content-type": "message/rfc822",
        "x-mail-timestamp": String(timestamp),
        "x-mail-nonce": nonce,
        "x-mail-signature": signature,
        "x-envelope-from": envelopeFrom ?? "",
        "x-envelope-to": envelopeTo,
      },
      body: rawMime,
    });
    if (!response.ok) {
      message.setReject("provisioning email ingest rejected the message");
      return;
    }
    if (enabled(env.DEBUG_FORWARD_ENABLED) && env.DEBUG_FORWARD_ADDRESS) {
      // This is deliberately disabled unless an operator explicitly enables it.
      ctx.waitUntil(message.forward(env.DEBUG_FORWARD_ADDRESS));
    }
  },
};
