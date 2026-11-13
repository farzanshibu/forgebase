import type { Transport } from "./http";
import type { Schemas } from "./types";

/*
 * Product customization per realm (`/api/v1/customize`): custom domains, white-label branding, email templates and the
 * delivery log, legal agreements and acceptance, and the public views a hosted sign-in page needs: `client.customize`.
 * Every admin call acts on the caller's realm; platform administrators pass `{ realm }` to act on another.
 */

export type CustomDomain = Schemas["Domain"];
export type DomainVerification = Schemas["Verification"];
export type DomainVerifyResult = Schemas["VerifyResult"];
export type Branding = Schemas["Branding"];
export type BrandingInput = Schemas["BrandingInput"];
export type PublicBranding = Schemas["PublicBranding"];
export type EmailTemplate = Schemas["EmailTemplate"];
export type EmailTemplateInput = Schemas["TemplateInput"];
export type EmailPreview = Schemas["Preview"];
export type EmailDelivery = Schemas["Delivery"];
export type EmailDeliveryStatus = Schemas["DeliveryStatus"];
export type Agreement = Schemas["Agreement"];
export type AgreementRef = Schemas["AgreementRef"];
export type AgreementAcceptance = Schemas["Acceptance"];
export type AcceptItem = Schemas["AcceptItem"];

/** `logo` or `favicon`. */
export type BrandingAssetKind = "logo" | "favicon";

export interface RealmScope {
  /** Platform administrators: the realm to act on (default: the realm of the request). */
  realm?: string;
}

export interface EmailDeliveriesQuery extends RealmScope {
  status?: EmailDeliveryStatus;
  /** Recipient address. */
  to?: string;
  /** Template key. */
  template?: string;
  /** Default 50, at most 200. */
  limit?: number;
  /** Only rows created before this time (epoch ms). */
  before?: number;
}

export interface AssetUpload extends RealmScope {
  /** `image/png`, `image/jpeg`, ... (default: the `Blob`'s own type, else `image/png`). At most 256 KiB. */
  contentType?: string;
}

const seg = encodeURIComponent;
const realmQ = (o: RealmScope | undefined) => ({ realm: o?.realm });
const listOf = async <T>(p: Promise<{ items: T[] }>): Promise<T[]> => (await p).items;

/** Domains routed to a realm by `Host` header, verified with a DNS TXT record: `customize.domains`. */
export class DomainsApi {
  constructor(private readonly t: Transport) {}

  list(o: RealmScope = {}): Promise<CustomDomain[]> {
    return listOf(this.t.request("/api/v1/customize/domains", { query: realmQ(o) }));
  }

  /** Adds a pending domain; publish the returned `verification` record in DNS, then `verify`. `host` has no scheme, port or path. */
  add(host: string, o: RealmScope = {}): Promise<CustomDomain> {
    return this.t.request("/api/v1/customize/domains", { method: "POST", body: { host, realm: o.realm } });
  }

  get(id: string): Promise<CustomDomain> {
    return this.t.request(`/api/v1/customize/domains/${seg(id)}`);
  }

  delete(id: string): Promise<void> {
    return this.t.request(`/api/v1/customize/domains/${seg(id)}`, { method: "DELETE" });
  }

  /** Checks the DNS record. `{ verified: false, reason }` when it is not there yet; an expired token is a `ConflictError`. */
  verify(id: string): Promise<DomainVerifyResult> {
    return this.t.request(`/api/v1/customize/domains/${seg(id)}/verify`, { method: "POST", body: {} });
  }

  /** New verification token after the old one expired. */
  reissue(id: string): Promise<CustomDomain> {
    return this.t.request(`/api/v1/customize/domains/${seg(id)}/reissue`, { method: "POST", body: {} });
  }
}

/** White-label branding: `customize.branding`. */
export class BrandingApi {
  constructor(private readonly t: Transport) {}

  get(o: RealmScope = {}): Promise<Branding> {
    return this.t.request("/api/v1/customize/branding", { query: realmQ(o) });
  }

  /** Replaces the branding: a field left out is cleared. Images go through `uploadAsset`. */
  put(input: BrandingInput, o: RealmScope = {}): Promise<Branding> {
    return this.t.request("/api/v1/customize/branding", { method: "PUT", body: input, query: realmQ(o) });
  }

  /** Uploads the logo or favicon (an image of at most 256 KiB). */
  uploadAsset(kind: BrandingAssetKind, data: Blob | ArrayBuffer | Uint8Array, o: AssetUpload = {}): Promise<Branding> {
    const contentType = o.contentType ?? (data instanceof Blob && data.type ? data.type : "image/png");
    return this.t.request(`/api/v1/customize/branding/assets/${seg(kind)}`, {
      method: "PUT",
      rawBody: data as BodyInit,
      headers: { "Content-Type": contentType },
      query: realmQ(o),
    });
  }

  deleteAsset(kind: BrandingAssetKind, o: RealmScope = {}): Promise<Branding> {
    return this.t.request(`/api/v1/customize/branding/assets/${seg(kind)}`, { method: "DELETE", query: realmQ(o) });
  }
}

/** Transactional email: templates, preview, sending and the delivery log: `customize.email`. */
export class EmailApi {
  constructor(private readonly t: Transport) {}

  /** Built-in templates plus the realm's overrides and custom keys. */
  templates(o: RealmScope = {}): Promise<EmailTemplate[]> {
    return listOf(this.t.request("/api/v1/customize/email/templates", { query: realmQ(o) }));
  }

  template(key: string, o: RealmScope = {}): Promise<EmailTemplate> {
    return this.t.request(`/api/v1/customize/email/templates/${seg(key)}`, { query: realmQ(o) });
  }

  /** Creates or replaces the realm's template for `key` (`verify_email`, `password_reset`, `magic_link`, `email_otp`, `invitation`, or your own). */
  putTemplate(key: string, input: EmailTemplateInput, o: RealmScope = {}): Promise<EmailTemplate> {
    return this.t.request(`/api/v1/customize/email/templates/${seg(key)}`, { method: "PUT", body: input, query: realmQ(o) });
  }

  /** Removes the override: a built-in key falls back to the built-in template. */
  deleteTemplate(key: string, o: RealmScope = {}): Promise<void> {
    return this.t.request(`/api/v1/customize/email/templates/${seg(key)}`, { method: "DELETE", query: realmQ(o) });
  }

  /** Renders a saved template (`template`) with `variables`; `missing` lists the variables it needs and did not get. */
  preview(input: { template?: string; variables?: Record<string, unknown> }, o: RealmScope = {}): Promise<EmailPreview> {
    return this.t.request("/api/v1/customize/email/preview", { method: "POST", body: input, query: realmQ(o) });
  }

  /** Queues a message from a template (delivery is asynchronous, see `delivery`). */
  send(input: { template: string; to: string; variables?: Record<string, unknown> }, o: RealmScope = {}): Promise<EmailDelivery> {
    return this.t.request("/api/v1/customize/email/send", { method: "POST", body: input, query: realmQ(o) });
  }

  /** Delivery log, newest first. */
  deliveries(q: EmailDeliveriesQuery = {}): Promise<EmailDelivery[]> {
    return listOf(
      this.t.request("/api/v1/customize/email/deliveries", {
        query: { realm: q.realm, status: q.status, to: q.to, template: q.template, limit: q.limit, before: q.before },
      }),
    );
  }

  delivery(id: string): Promise<EmailDelivery> {
    return this.t.request(`/api/v1/customize/email/deliveries/${seg(id)}`);
  }

  /** Queues a failed delivery again. */
  retry(id: string): Promise<EmailDelivery> {
    return this.t.request(`/api/v1/customize/email/deliveries/${seg(id)}/retry`, { method: "POST", body: {} });
  }
}

/** Versioned legal agreements and who accepted which version: `customize.agreements`. */
export class AgreementsApi {
  constructor(private readonly t: Transport) {}

  /** Every version (newest first); `kind` narrows to `terms`, `privacy`, ... */
  list(o: RealmScope & { kind?: string } = {}): Promise<Agreement[]> {
    return listOf(this.t.request("/api/v1/customize/agreements", { query: { realm: o.realm, kind: o.kind } }));
  }

  /** Publishes the next version of an agreement (`kind`: lower-case letters, digits, `_`, `-`). Users must accept it again. */
  publish(input: { kind: string; title: string; body: string; url?: string }, o: RealmScope = {}): Promise<Agreement> {
    return this.t.request("/api/v1/customize/agreements", { method: "POST", body: input, query: realmQ(o) });
  }

  /** Current versions the caller has not accepted yet. */
  async pending(): Promise<AgreementRef[]> {
    return (await this.t.request<{ pending: AgreementRef[] }>("/api/v1/customize/agreements/pending")).pending;
  }

  /** Records the caller's acceptance. An outdated version is a `ConflictError` (`agreement_outdated`). */
  accept(items: AcceptItem[]): Promise<AgreementAcceptance[]> {
    return listOf(this.t.request("/api/v1/customize/agreements/accept", { method: "POST", body: { items } }));
  }

  /** The caller's own acceptance records. */
  accepted(): Promise<AgreementAcceptance[]> {
    return listOf(this.t.request("/api/v1/customize/agreements/accepted"));
  }

  /** Acceptance records of the realm (administrators); `principal` is `user:<collection>/<id>`. */
  acceptances(o: RealmScope & { principal?: string; kind?: string; limit?: number } = {}): Promise<AgreementAcceptance[]> {
    return listOf(
      this.t.request("/api/v1/customize/agreements/acceptances", { query: { realm: o.realm, principal: o.principal, kind: o.kind, limit: o.limit } }),
    );
  }
}

/**
 * What a hosted sign-in page or a dashboard shows before anyone signs in. No credentials are sent. On a verified
 * custom domain the realm is the domain's; elsewhere pass `realm` (default: the request's).
 */
export class PublicCustomizeApi {
  constructor(private readonly t: Transport) {}

  branding(o: RealmScope = {}): Promise<PublicBranding> {
    return this.t.request("/api/v1/customize/public/branding", { query: realmQ(o), anonymous: true });
  }

  /** Current agreement versions, with their text. */
  agreements(o: RealmScope = {}): Promise<Agreement[]> {
    return listOf(this.t.request("/api/v1/customize/public/agreements", { query: realmQ(o), anonymous: true }));
  }

  /** URL of the logo or favicon for an `<img src>`. A `NotFoundError` (404) means none is uploaded, when fetched. */
  assetUrl(kind: BrandingAssetKind, o: RealmScope = {}): string {
    return this.t.url(`/api/v1/customize/public/branding/${seg(kind)}`, realmQ(o));
  }

  /** Downloads the logo or favicon. */
  async asset(kind: BrandingAssetKind, o: RealmScope = {}): Promise<Blob> {
    const res = await this.t.request<Response>(`/api/v1/customize/public/branding/${seg(kind)}`, { query: realmQ(o), anonymous: true, raw: true });
    return res.blob();
  }

  /** URL of the CSS custom properties (`--fb-primary`, ...) of the branding, for a `<link rel="stylesheet">`. */
  themeUrl(o: RealmScope = {}): string {
    return this.t.url("/api/v1/customize/public/theme.css", realmQ(o));
  }

  theme(o: RealmScope = {}): Promise<string> {
    return this.t.request("/api/v1/customize/public/theme.css", { query: realmQ(o), anonymous: true, text: true, headers: { Accept: "text/css" } });
  }
}

/** Customization of the realm's look, mail and legal text: `client.customize`. */
export class CustomizeApi {
  readonly domains: DomainsApi;
  readonly branding: BrandingApi;
  readonly email: EmailApi;
  readonly agreements: AgreementsApi;
  /** Unauthenticated views for hosted pages. */
  readonly hosted: PublicCustomizeApi;

  constructor(private readonly t: Transport) {
    this.domains = new DomainsApi(t);
    this.branding = new BrandingApi(t);
    this.email = new EmailApi(t);
    this.agreements = new AgreementsApi(t);
    this.hosted = new PublicCustomizeApi(t);
  }

  /** URL of the branded API documentation page (HTML), for a link or an iframe. */
  docsUrl(o: RealmScope = {}): string {
    return this.t.url("/api/v1/customize/docs", realmQ(o));
  }

  /** The branded API documentation page as HTML. */
  docs(o: RealmScope = {}): Promise<string> {
    return this.t.request("/api/v1/customize/docs", { query: realmQ(o), anonymous: true, text: true, headers: { Accept: "text/html" } });
  }
}
