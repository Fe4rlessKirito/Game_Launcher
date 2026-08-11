import assert from "node:assert/strict";
import { createHmac, createHash } from "node:crypto";
import test from "node:test";

test("canonical payload and HMAC match the Rust verifier contract", () => {
  const body = Buffer.from("raw MIME fixture");
  const bodySha256 = createHash("sha256").update(body).digest("hex");
  const canonical = [
    "1700000000",
    "nonce-1",
    bodySha256,
    "sender@example.test",
    "p-example@vaultnode.pp.ua",
  ].join("\n");
  const signature = createHmac("sha256", "shared-secret")
    .update(canonical)
    .digest("base64");
  assert.equal(signature, "kLdRMZ2obwtjgZoPTQZgua9z6ry175ktH87n2kI/Qzs=");
});
