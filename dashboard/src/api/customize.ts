/** Typed client for `/api/v1/customize` (ADR-0020). Hand-written; the OpenAPI fixture does not describe these routes yet. Times are epoch milliseconds. */
import { request, send } from "./client";

const C = "/api/v1/customize";
const enc = encodeURIComponent;
const q = (realm?: string, extra: Record<string, string | number | undefined> = {}) => {
  const p = new URLSearchParams();
  if (realm) p.set("realm", realm);
  for (const [k, v] of Object.entries(extra)) if (v !== undefined && v !== "") p.set(k, String(v));
  const s = p.toString();
  return s ? `?${s}` : "";
};

export type DomainStatus = "pending" | "verified";
export interface Verification { record_name: string; record_type: string; record_value: string; expires_at: number }
export interface Domain {
  id: string; realm: string; host: string; status: DomainStatus; verification: Verification | null; created_at: number;
  verified_at: number | null; last_checked: number | null; last_error: string | null; created_by: string;
}
export interface VerifyResult { verified: boolean; reason: string | null; domain: Domain }

export interface Branding {
  realm: string; display_name: string | null; primary_color: string | null; background_color: string | null; text_color: string | null; accent_color: string | null;
  signin_title: string | null; signin_subtitle: string | null; signin_footer: string | null; support_email: string | null;
  has_logo: boolean; has_favicon: boolean; updated_at: number | null;
}
export type BrandingInput = Partial<Omit<Branding, "realm" | "has_logo" | "has_favicon" | "updated_at">>;
export type AssetKind = "logo" | "favicon";
/** An image is at most 256 KiB. */
export const MAX_ASSET_BYTES = 256 * 1024;

export interface EmailTemplate { realm: string; key: string; subject: string; text: string; html: string | null; customized: boolean; variables: string[]; updated_at: number | null }
export interface TemplateInput { subject: string; text: string; html?: string | null }
export interface Preview { subject: string; text: string; html: string | null; missing: string[] }
export type DeliveryStatus = "queued" | "retrying" | "sent" | "failed";
export const DELIVERY_STATUSES: DeliveryStatus[] = ["queued", "retrying", "sent", "failed"];
export interface Delivery {
  id: string; realm: string; template: string | null; to: string; subject: string; status: DeliveryStatus; attempts: number; last_error: string | null;
  provider: string | null; job_id: string | null; created_at: number; updated_at: number; sent_at: number | null;
}

export interface Agreement { id: string; realm: string; kind: string; version: number; title: string; body: string; url: string | null; published_at: number; published_by: string }
export interface AgreementInput { kind: string; title: string; body: string; url?: string }
export interface Acceptance { id: string; realm: string; principal: string; kind: string; version: number; accepted_at: number; ip: string | null; user_agent: string | null }

export const CUSTOMIZE_OPERATIONS = [
  "get /api/v1/customize/domains", "post /api/v1/customize/domains", "delete /api/v1/customize/domains/{id}", "post /api/v1/customize/domains/{id}/verify",
  "post /api/v1/customize/domains/{id}/reissue", "get /api/v1/customize/branding", "put /api/v1/customize/branding",
  "put /api/v1/customize/branding/assets/{kind}", "delete /api/v1/customize/branding/assets/{kind}", "get /api/v1/customize/email/templates",
  "put /api/v1/customize/email/templates/{key}", "delete /api/v1/customize/email/templates/{key}", "post /api/v1/customize/email/preview",
  "post /api/v1/customize/email/send", "get /api/v1/customize/email/deliveries", "post /api/v1/customize/email/deliveries/{id}/retry",
  "get /api/v1/customize/agreements", "post /api/v1/customize/agreements", "get /api/v1/customize/agreements/acceptances", "get /api/v1/customize/public/branding",
] as const;

const items = async <T>(path: string, signal?: AbortSignal) => (await request<{ items: T[] }>(path, { signal })).items;

export const listDomains = (realm?: string, s?: AbortSignal) => items<Domain>(`${C}/domains${q(realm)}`, s);
export const addDomain = (host: string, realm?: string) => send<Domain>("POST", `${C}/domains`, realm ? { host, realm } : { host });
export const deleteDomain = (id: string) => send<void>("DELETE", `${C}/domains/${enc(id)}`);
export const verifyDomain = (id: string) => send<VerifyResult>("POST", `${C}/domains/${enc(id)}/verify`, {});
export const reissueDomain = (id: string) => send<Domain>("POST", `${C}/domains/${enc(id)}/reissue`, {});

export const getBranding = (realm?: string, s?: AbortSignal) => request<Branding>(`${C}/branding${q(realm)}`, { signal: s });
export const putBranding = (b: BrandingInput, realm?: string) => send<Branding>("PUT", `${C}/branding${q(realm)}`, b);
export const putBrandingAsset = (kind: AssetKind, file: Blob, realm?: string) =>
  request<Branding>(`${C}/branding/assets/${kind}${q(realm)}`, { method: "PUT", rawBody: file, headers: { "Content-Type": file.type || "application/octet-stream" } });
export const deleteBrandingAsset = (kind: AssetKind, realm?: string) => send<Branding>("DELETE", `${C}/branding/assets/${kind}${q(realm)}`);
/** URL of the public image (served without credentials). */
export const brandingAssetUrl = (kind: AssetKind, realm?: string) => `${C}/public/branding/${kind}${q(realm)}`;

export const listTemplates = (realm?: string, s?: AbortSignal) => items<EmailTemplate>(`${C}/email/templates${q(realm)}`, s);
export const putTemplate = (key: string, t: TemplateInput, realm?: string) => send<EmailTemplate>("PUT", `${C}/email/templates/${enc(key)}${q(realm)}`, t);
/** Back to the built-in default. */
export const resetTemplate = (key: string, realm?: string) => send<void>("DELETE", `${C}/email/templates/${enc(key)}${q(realm)}`);
export const previewEmail = (body: { template?: string; draft?: TemplateInput; variables: Record<string, string> }, realm?: string) =>
  send<Preview>("POST", `${C}/email/preview${q(realm)}`, body);
export const sendTestEmail = (body: { template: string; to: string; variables: Record<string, string> }, realm?: string) =>
  send<Delivery>("POST", `${C}/email/send${q(realm)}`, body);
export const listDeliveries = (f: { realm?: string; status?: DeliveryStatus | ""; to?: string; template?: string; limit?: number } = {}, s?: AbortSignal) =>
  items<Delivery>(`${C}/email/deliveries${q(f.realm, { status: f.status, to: f.to, template: f.template, limit: f.limit })}`, s);
export const retryDelivery = (id: string) => send<Delivery>("POST", `${C}/email/deliveries/${enc(id)}/retry`, {});

export const listAgreements = (realm?: string, kind?: string, s?: AbortSignal) => items<Agreement>(`${C}/agreements${q(realm, { kind })}`, s);
export const publishAgreement = (a: AgreementInput, realm?: string) => send<Agreement>("POST", `${C}/agreements${q(realm)}`, a);
export const listAcceptances = (f: { realm?: string; kind?: string; principal?: string; limit?: number } = {}, s?: AbortSignal) =>
  items<Acceptance>(`${C}/agreements/acceptances${q(f.realm, { kind: f.kind, principal: f.principal, limit: f.limit })}`, s);
