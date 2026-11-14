/** Helpers of the Webhook deliveries page (ADR-0032): outcome tones, the stored body, links to touched objects and the words for a replay result. */
import type { Tone } from "../../components/StatusBadge";
import type { WebhookDeliveryDetail, WebhookDeliveryResult, WebhookLinks } from "../../api";

/** Providers the inspector knows by name; any other is stored as `other`. */
export const WEBHOOK_PROVIDERS = ["stripe", "razorpay", "wechat_pay", "manual", "app_store", "google_play", "creem", "other"];

export const OUTCOME_TONE: Record<string, Tone> = {
  applied: "ok", ignored: "neutral", duplicate: "info", failed: "bad",
  signature_invalid: "bad", timestamp_out_of_tolerance: "warn", unknown_provider: "warn", malformed: "warn", rejected: "bad", error: "bad",
};
export const OUTCOME_WORDS: Record<string, string> = {
  applied: "applied", ignored: "ignored", duplicate: "duplicate", failed: "failed", signature_invalid: "signature invalid",
  timestamp_out_of_tolerance: "timestamp out of tolerance", unknown_provider: "unknown provider", malformed: "malformed", rejected: "rejected", error: "error",
};
/** What each refusal means and what to do, for the drawer. */
export const OUTCOME_HELP: Record<string, string> = {
  signature_invalid: "The signature did not match any webhook secret. Check the secret in Providers and settings, then Re-verify.",
  timestamp_out_of_tolerance: "The signed time is too far from now: an old delivery was replayed, or the clocks differ.",
  unknown_provider: "No such provider, or it is not configured on this server.",
  malformed: "The body or a required header could not be read.",
  rejected: "Refused for another reason: another app or merchant, live mode, or a bad certificate.",
  error: "Verification could not complete: the store's API or the database failed. Re-verify to try again.",
};

/** The delivery was refused (not verified): it can be re-verified, not replayed. */
export const isRefused = (d: { verification: string }) => d.verification !== "verified";

/** The stored body as text to show: JSON is pretty-printed, other text as it is, base64 as the bytes' base64 with a count. */
export function bodyView(d: Pick<WebhookDeliveryDetail, "body" | "body_encoding" | "body_stored" | "body_size">): { kind: "none" | "text" | "json" | "base64"; text: string; bytes?: number } {
  if (d.body === null || d.body === undefined || !d.body_stored) return { kind: "none", text: "" };
  if (d.body_encoding === "base64") {
    const pad = (d.body.match(/=+$/)?.[0].length ?? 0);
    return { kind: "base64", text: d.body, bytes: Math.max(0, Math.floor((d.body.length * 3) / 4) - pad) };
  }
  try {
    return { kind: "json", text: JSON.stringify(JSON.parse(d.body), null, 2) };
  } catch {
    return { kind: "text", text: d.body };
  }
}

const LINK_ROUTES: Record<keyof WebhookLinks, { label: string; to: (id: string) => string } | undefined> = {
  webhook_event_id: undefined,
  customer_id: { label: "Customer", to: () => "/billing/customers" },
  subscription_id: { label: "Subscription", to: () => "/billing/subscriptions" },
  order_id: { label: "Order", to: () => "/billing/orders" },
  invoice_id: { label: "Invoice", to: (id) => `/billing/invoices/${encodeURIComponent(id)}` },
  payment_id: { label: "Payment", to: () => "/billing/payments" },
  refund_id: { label: "Refund", to: () => "/billing/payments" },
  store_purchase_id: { label: "Store purchase", to: () => "/billing/stores" },
};

/** The billing objects a delivery touched, each with where to look at it. The event dedup row has no page: it is listed without a link. */
export function touchedObjects(links: WebhookLinks): { key: string; label: string; id: string; to?: string }[] {
  return (Object.keys(LINK_ROUTES) as (keyof WebhookLinks)[])
    .filter((k) => links[k])
    .map((k) => {
      const r = LINK_ROUTES[k];
      return { key: k, label: r?.label ?? "Webhook event", id: links[k]!, to: r?.to(links[k]!) };
    });
}

export interface ResultWords { tone: "ok" | "info" | "warn" | "bad"; title: string; text: string }

/** A replay or re-verify answer in plain words. */
export function resultWords(r: Pick<WebhookDeliveryResult, "result" | "detail">): ResultWords {
  const detail = r.detail ? ` (${r.detail})` : "";
  switch (r.result) {
    case "applied": return { tone: "ok", title: "Applied", text: `The event was processed now and changed billing state${detail}.` };
    case "ignored": return { tone: "info", title: "Ignored", text: `The event was processed now and recorded without effect${detail}.` };
    case "failed": return { tone: "bad", title: "Failed", text: `Processing failed again${detail}. The provider's redelivery or another replay can retry it.` };
    case "already_applied": return { tone: "info", title: "Already applied", text: "This event was handled before, so nothing changed." };
    case "already_ignored": return { tone: "info", title: "Already ignored", text: "This event was ignored before, so nothing changed." };
    case "unverified": return { tone: "warn", title: "Still does not verify", text: `The stored body does not verify with the current webhook secrets, so nothing was processed${detail}. Fix the secret in Providers and settings and re-verify.` };
    default: return { tone: "info", title: r.result, text: r.detail ?? "Done." };
  }
}
