import { describe, expect, it } from "vitest";
import { ConflictError, ForgeBase } from "../src/index";
import { errBody, json, scripted } from "./helpers";

const purchase = { id: "sp1", provider: "app_store", purchase_id: "orig1", subject: "usr_1", status: "active", kind: "subscription", price: "pro_month", product_id: "com.x.pro", environment: "test", auto_renew: true, created_at: "t", updated_at: "t" };
const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  return { f, b: new ForgeBase("http://x", { fetch: f.fetch }).billing.store };
};

describe("billing.store", () => {
  it("verify maps a receipt to the wire body and leaves unset fields out", async () => {
    const { f, b } = setup(json(200, purchase), json(200, { ...purchase, provider: "google_play" }));
    expect(await b.verify("app_store", { signedTransaction: "jws", subject: "org:1" })).toMatchObject({ id: "sp1" });
    await b.verify("google_play", { purchaseToken: "tok", productId: "pro", kind: "one_time" });
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/billing/store/app_store/verify" });
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({ signed_transaction: "jws", subject: "org:1" });
    expect(f.calls[1]!.url).toBe("http://x/api/v1/billing/store/google_play/verify");
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ purchase_token: "tok", product_id: "pro", kind: "one_time" });
  });

  it("surfaces purchase_owned as a ConflictError", async () => {
    const { b } = setup(json(409, errBody("purchase_owned")));
    await expect(b.verify("app_store", { transactionId: "1" })).rejects.toBeInstanceOf(ConflictError);
  });

  it("checkout sends the price, return url and idempotency key", async () => {
    const { f, b } = setup(json(200, { session_id: "co_1", url: "https://pay.creem.io/x" }));
    expect(await b.checkout("creem", { price: "pro_month", successUrl: "https://app/ok", idempotencyKey: "k1" })).toEqual({ session_id: "co_1", url: "https://pay.creem.io/x" });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/billing/store/creem/checkout");
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({ price: "pro_month", success_url: "https://app/ok", idempotency_key: "k1" });
  });

  it("purchases unwraps the items and passes the subject", async () => {
    const { f, b } = setup(json(200, { items: [purchase] }), json(200, { items: [] }));
    expect(await b.purchases()).toHaveLength(1);
    expect(await b.purchases("org:9")).toEqual([]);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/billing/store-purchases");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/billing/store-purchases?subject=org%3A9");
  });
});
