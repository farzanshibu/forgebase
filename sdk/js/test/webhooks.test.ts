import { createHmac } from "node:crypto";
import { describe, expect, it } from "vitest";
import { WebhookVerificationError, constructWebhookEvent, signWebhook, verifyWebhookSignature } from "../src/index";

/** Independent reference: what the server's `sign` (crates/forgebase-queue/src/push.rs) computes. */
const ref = (secret: string, ts: number, body: string | Buffer) =>
  createHmac("sha256", secret).update(`${ts}.`).update(body).digest("hex");

const SECRET = "whsec_4f1c9d2a7b";
const T = 1_700_000_000;
const NOW = new Date((T + 10) * 1000);
const envelope = {
  subscription: "orders-push",
  delivery_attempt: 2,
  message: {
    id: "msg_1",
    topic: "orders",
    seq: 7,
    data: { order: 42, note: "héllo ✓" },
    attributes: { kind: "created" },
    publisher: "usr_1",
    bytes: 31,
    published_at: "2026-01-01T00:00:00Z",
  },
};
const BODY = JSON.stringify(envelope);
const header = (secret = SECRET, ts = T, body: string | Buffer = BODY) => `t=${ts},v1=${ref(secret, ts, body)}`;

describe("verifyWebhookSignature", () => {
  it("matches a vector computed with openssl (key is the whole secret string, prefix included)", async () => {
    // printf '1700000000.{"a":1}' | openssl dgst -sha256 -hmac 'whsec_test'
    const h = "t=1700000000,v1=38877139021993b830af32feea6e18a8da83eb2f6e49ee50bd9e4cf4ca4d3789";
    expect(await verifyWebhookSignature({ secret: "whsec_test", header: h, body: '{"a":1}', now: NOW })).toBe(true);
    expect(await verifyWebhookSignature({ secret: "test", header: h, body: '{"a":1}', now: NOW })).toBe(false);
    expect(await signWebhook({ secret: "whsec_test", body: '{"a":1}', timestamp: 1_700_000_000 })).toBe(h);
  });

  it("accepts a valid signature over a string, Uint8Array, Buffer and ArrayBuffer body", async () => {
    const h = header();
    const bytes = new TextEncoder().encode(BODY);
    for (const body of [BODY, bytes, Buffer.from(BODY), bytes.buffer.slice(0) as ArrayBuffer]) {
      expect(await verifyWebhookSignature({ secret: SECRET, header: h, body, now: NOW })).toBe(true);
    }
  });

  it("signs the UTF-8 bytes, also for a view into a larger buffer", async () => {
    const h = header();
    const pad = Buffer.concat([Buffer.from("xx"), Buffer.from(BODY), Buffer.from("yy")]);
    const view = new Uint8Array(pad.buffer, pad.byteOffset + 2, Buffer.byteLength(BODY));
    expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: view, now: NOW })).toBe(true);
  });

  it("rejects a tampered body", async () => {
    const h = header();
    for (const body of [BODY.replace("42", "43"), `${BODY} `, JSON.stringify(JSON.parse(BODY), null, 1), ""]) {
      expect(await verifyWebhookSignature({ secret: SECRET, header: h, body, now: NOW })).toBe(false);
    }
  });

  it("rejects a tampered timestamp", async () => {
    const sig = ref(SECRET, T, BODY);
    expect(await verifyWebhookSignature({ secret: SECRET, header: `t=${T + 1},v1=${sig}`, body: BODY, now: NOW })).toBe(false);
    expect(await verifyWebhookSignature({ secret: SECRET, header: `t=${T - 1},v1=${sig}`, body: BODY, now: NOW })).toBe(false);
  });

  it("rejects the wrong secret and a flipped signature bit", async () => {
    expect(await verifyWebhookSignature({ secret: "whsec_other", header: header(), body: BODY, now: NOW })).toBe(false);
    const sig = ref(SECRET, T, BODY);
    const flipped = (sig[0] === "0" ? "1" : "0") + sig.slice(1);
    expect(await verifyWebhookSignature({ secret: SECRET, header: `t=${T},v1=${flipped}`, body: BODY, now: NOW })).toBe(false);
    expect(await verifyWebhookSignature({ secret: SECRET, header: `t=${T},v1=${sig.slice(0, -2)}`, body: BODY, now: NOW })).toBe(false);
    expect(await verifyWebhookSignature({ secret: SECRET, header: `t=${T},v1=${sig}00`, body: BODY, now: NOW })).toBe(false);
  });

  it("accepts uppercase hex", async () => {
    const h = `t=${T},v1=${ref(SECRET, T, BODY).toUpperCase()}`;
    expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: BODY, now: NOW })).toBe(true);
  });

  describe("tolerance window", () => {
    const h = header();
    const at = (secs: number) => new Date((T + secs) * 1000);
    it("defaults to 300 seconds in both directions", async () => {
      expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: BODY, now: at(300) })).toBe(true);
      expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: BODY, now: at(301) })).toBe(false);
      expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: BODY, now: at(-300) })).toBe(true);
      expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: BODY, now: at(-301) })).toBe(false);
    });
    it("honours toleranceSeconds", async () => {
      expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: BODY, now: at(60), toleranceSeconds: 60 })).toBe(true);
      expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: BODY, now: at(61), toleranceSeconds: 60 })).toBe(false);
      expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: BODY, now: at(0), toleranceSeconds: 0 })).toBe(true);
      expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: BODY, now: at(1), toleranceSeconds: 0 })).toBe(false);
      expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: BODY, now: at(10 ** 7), toleranceSeconds: Infinity })).toBe(true);
    });
    it("takes now as a Date or as milliseconds", async () => {
      expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: BODY, now: (T + 5) * 1000 })).toBe(true);
      expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: BODY, now: (T + 5000) * 1000 })).toBe(false);
    });
    it("uses the clock by default", async () => {
      const fresh = await signWebhook({ secret: SECRET, body: BODY });
      expect(await verifyWebhookSignature({ secret: SECRET, header: fresh, body: BODY })).toBe(true);
      expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: BODY })).toBe(false);
    });
    it("refuses a negative tolerance", async () => {
      await expect(verifyWebhookSignature({ secret: SECRET, header: h, body: BODY, now: NOW, toleranceSeconds: -1 })).rejects.toThrow(RangeError);
    });
  });

  describe("secret rotation", () => {
    const OLD = "whsec_old";
    const NEW = "whsec_new";
    const both = `t=${T},v1=${ref(NEW, T, BODY)},v1=${ref(OLD, T, BODY)}`;
    it("accepts a header with several v1 values when any matches", async () => {
      expect(await verifyWebhookSignature({ secret: NEW, header: both, body: BODY, now: NOW })).toBe(true);
      expect(await verifyWebhookSignature({ secret: OLD, header: both, body: BODY, now: NOW })).toBe(true);
      expect(await verifyWebhookSignature({ secret: "whsec_third", header: both, body: BODY, now: NOW })).toBe(false);
    });
    it("accepts a list of secrets on the receiving side", async () => {
      expect(await verifyWebhookSignature({ secret: [OLD, NEW], header: header(NEW), body: BODY, now: NOW })).toBe(true);
      expect(await verifyWebhookSignature({ secret: [OLD, NEW], header: header(OLD), body: BODY, now: NOW })).toBe(true);
      expect(await verifyWebhookSignature({ secret: ["whsec_a", "whsec_b"], header: both, body: BODY, now: NOW })).toBe(false);
      expect(await verifyWebhookSignature({ secret: [], header: both, body: BODY, now: NOW })).toBe(false);
    });
    it("skips undecodable v1 values and unknown schemes", async () => {
      const h = `t=${T},v1=nothex,v1=,v2=abcd,v1=${ref(NEW, T, BODY)}`;
      expect(await verifyWebhookSignature({ secret: NEW, header: h, body: BODY, now: NOW })).toBe(true);
    });
    it("still checks the timestamp when one v1 matches", async () => {
      expect(await verifyWebhookSignature({ secret: NEW, header: both, body: BODY, now: new Date((T + 400) * 1000) })).toBe(false);
    });
  });

  it("rejects missing and malformed headers (false, never a throw)", async () => {
    const sig = ref(SECRET, T, BODY);
    const bad = [
      null,
      undefined,
      "",
      "   ",
      "garbage",
      `v1=${sig}`,
      `t=${T}`,
      `t=${T},v2=${sig}`,
      `t=abc,v1=${sig}`,
      `t=-5,v1=${sig}`,
      `t=${T}.5,v1=${sig}`,
      `t=${T},t=${T},v1=${sig}`,
      `t=99999999999999999999,v1=${sig}`,
    ];
    for (const header of bad) {
      expect(await verifyWebhookSignature({ secret: SECRET, header, body: BODY, now: NOW }), String(header)).toBe(false);
    }
  });

  it("tolerates whitespace around the parts", async () => {
    const h = ` t=${T} , v1=${ref(SECRET, T, BODY)} `;
    expect(await verifyWebhookSignature({ secret: SECRET, header: h, body: BODY, now: NOW })).toBe(true);
  });

  it("refuses a parsed object as the body", async () => {
    await expect(verifyWebhookSignature({ secret: SECRET, header: header(), body: envelope as unknown as string, now: NOW })).rejects.toThrow(TypeError);
  });
});

describe("signWebhook", () => {
  it("round-trips with the reference HMAC", async () => {
    const h = await signWebhook({ secret: SECRET, body: new TextEncoder().encode(BODY), timestamp: T });
    expect(h).toBe(header());
  });
  it("rejects a bad timestamp", async () => {
    await expect(signWebhook({ secret: SECRET, body: BODY, timestamp: 1.5 })).rejects.toThrow(RangeError);
    await expect(signWebhook({ secret: SECRET, body: BODY, timestamp: -1 })).rejects.toThrow(RangeError);
  });
});

describe("constructWebhookEvent", () => {
  it("returns the typed envelope after verifying", async () => {
    const ev = await constructWebhookEvent<{ order: number; note: string }>({ secret: SECRET, header: header(), body: BODY, now: NOW });
    expect(ev.subscription).toBe("orders-push");
    expect(ev.delivery_attempt).toBe(2);
    expect(ev.message.id).toBe("msg_1");
    expect(ev.message.data.order).toBe(42);
    expect(ev.message.data.note).toBe("héllo ✓");
  });

  it("reads bytes the same way", async () => {
    const buf = Buffer.from(BODY);
    const ev = await constructWebhookEvent({ secret: SECRET, header: header(SECRET, T, buf), body: buf, now: NOW });
    expect(ev.message.seq).toBe(7);
  });

  const reasonOf = async (opts: Parameters<typeof constructWebhookEvent>[0]) => {
    try {
      await constructWebhookEvent(opts);
    } catch (e) {
      expect(e).toBeInstanceOf(WebhookVerificationError);
      return (e as WebhookVerificationError).reason;
    }
    return "no error";
  };

  it("throws WebhookVerificationError with the reason", async () => {
    expect(await reasonOf({ secret: SECRET, header: undefined, body: BODY, now: NOW })).toBe("missing_header");
    expect(await reasonOf({ secret: SECRET, header: "nope", body: BODY, now: NOW })).toBe("malformed_header");
    expect(await reasonOf({ secret: SECRET, header: header(), body: BODY, now: new Date((T + 1000) * 1000) })).toBe("timestamp_out_of_tolerance");
    expect(await reasonOf({ secret: SECRET, header: header(), body: `${BODY}x`, now: NOW })).toBe("signature_mismatch");
    expect(await reasonOf({ secret: "whsec_other", header: header(), body: BODY, now: NOW })).toBe("signature_mismatch");
  });

  it("carries a stable code and is a ForgeBaseError", async () => {
    const err = await constructWebhookEvent({ secret: SECRET, header: "x", body: BODY, now: NOW }).catch((e: unknown) => e);
    expect(err).toMatchObject({ name: "WebhookVerificationError", code: "webhook_malformed_header", status: 0 });
  });

  it("rejects a correctly signed body that is not the envelope", async () => {
    for (const body of ["not json", "[]", "null", '{"subscription":"s"}', '{"subscription":"s","delivery_attempt":"1","message":{}}']) {
      expect(await reasonOf({ secret: SECRET, header: header(SECRET, T, body), body, now: NOW }), body).toBe("invalid_payload");
    }
    const invalidUtf8 = Buffer.from([0x7b, 0xff, 0x7d]);
    expect(await reasonOf({ secret: SECRET, header: header(SECRET, T, invalidUtf8), body: invalidUtf8, now: NOW })).toBe("invalid_payload");
  });

  it("never parses a body whose signature fails", async () => {
    // A bad signature is reported as such even when the body is not JSON.
    expect(await reasonOf({ secret: SECRET, header: header(), body: "not json", now: NOW })).toBe("signature_mismatch");
  });
});
