/**
 * Field tables and checks for the provider forms of Identity (ADR-0039): mail transports and SMS gateways.
 * Secret fields are write-only: the server answers `********` for a stored one and keeps it when the mask comes back.
 * The checks mirror the server's own (`invalid_mail_config`, SMS `bad base_url`) so a mistake shows next to its field before saving.
 */
import type { MailProviderConfig, MailProviderType, SmsConfig, SmsProviderType } from "../../api";

export interface GatewayField<K extends string> {
  key: K;
  label: string;
  secret?: boolean;
  required?: boolean;
  help?: string;
  placeholder?: string;
  /** A fixed list of values (a select). */
  options?: string[];
  kind?: "url" | "email" | "port";
}

type MailKey = Exclude<keyof MailProviderConfig, "provider" | "from_address">;
type SmsKey = Exclude<keyof SmsConfig, "provider">;

export const MAIL_PROVIDER_LABEL: Record<MailProviderType, string> = {
  smtp: "SMTP server", resend: "Resend", postmark: "Postmark", sendgrid: "SendGrid", mailgun: "Mailgun", ses: "Amazon SES", log: "Log only (development)",
};

const BASE_URL = (name: string): GatewayField<MailKey> => ({ key: "base_url", label: "Base URL", kind: "url", help: `Optional. Overrides the ${name} API address, for a proxy or a test server. Empty uses the provider's own.` });

export const MAIL_FIELDS: Record<MailProviderType, GatewayField<MailKey>[]> = {
  log: [],
  smtp: [
    { key: "host", label: "Host", required: true, placeholder: "smtp.example.com" },
    { key: "port", label: "Port", kind: "port", help: "Empty: 587 with STARTTLS, 465 with implicit TLS, 25 with none." },
    { key: "tls", label: "Encryption", options: ["starttls", "implicit", "none"], help: "STARTTLS (default), implicit TLS, or none for a trusted local relay." },
    { key: "username", label: "Username", help: "Set the username and the password together, or neither." },
    { key: "password", label: "Password", secret: true },
    { key: "envelope_from", label: "Envelope sender", required: true, kind: "email", help: "The MAIL FROM address, where bounces go." },
    { key: "helo", label: "HELO name", help: "Optional. The name announced to the server." },
  ],
  resend: [{ key: "api_key", label: "API key", secret: true, required: true }, BASE_URL("Resend")],
  postmark: [
    { key: "server_token", label: "Server token", secret: true, required: true },
    { key: "message_stream", label: "Message stream", help: "Letters, digits, - and _. Empty uses outbound." },
    BASE_URL("Postmark"),
  ],
  sendgrid: [
    { key: "api_key", label: "API key", secret: true, required: true },
    { key: "region", label: "Region", options: ["global", "eu"], help: "Use eu for an EU-resident SendGrid account." },
    BASE_URL("SendGrid"),
  ],
  mailgun: [
    { key: "api_key", label: "API key", secret: true, required: true },
    { key: "domain", label: "Sending domain", required: true, placeholder: "mg.example.com", help: "A domain you verified in Mailgun, such as mg.example.com." },
    { key: "region", label: "Region", options: ["us", "eu"], help: "Use eu for an EU-resident Mailgun account." },
    BASE_URL("Mailgun"),
  ],
  ses: [
    { key: "access_key_id", label: "Access key id", required: true },
    { key: "secret_access_key", label: "Secret access key", secret: true, required: true },
    { key: "session_token", label: "Session token", secret: true, help: "Optional. Only for temporary credentials." },
    { key: "region", label: "Region", required: true, placeholder: "eu-west-1" },
    { key: "configuration_set", label: "Configuration set", help: "Optional. An SES configuration set to apply to every message." },
    { key: "endpoint", label: "Endpoint", kind: "url", help: "Optional. Overrides the SES address." },
  ],
};

const SMS_BASE: GatewayField<SmsKey> = { key: "base_url", label: "Base URL", kind: "url", help: "Optional. Overrides the gateway's API address." };

export const SMS_PROVIDER_LABEL: Record<SmsProviderType, string> = {
  dev: "Development (writes codes to disk)", twilio: "Twilio", webhook: "HTTPS webhook", vonage: "Vonage", messagebird: "MessageBird", bird: "Bird (MessageBird's new platform)", sns: "Amazon SNS", plivo: "Plivo",
};

export const SMS_FIELDS: Record<SmsProviderType, GatewayField<SmsKey>[]> = {
  dev: [],
  twilio: [
    { key: "account_sid", label: "Account SID", required: true },
    { key: "from", label: "From number" },
    { key: "messaging_service_sid", label: "Messaging service SID" },
    { key: "auth_token", label: "Auth token", secret: true, required: true },
  ],
  webhook: [{ key: "url", label: "Webhook URL", required: true, kind: "url" }, { key: "bearer_token", label: "Bearer token", secret: true }],
  vonage: [
    { key: "api_key", label: "API key", secret: true, required: true },
    { key: "api_secret", label: "API secret", secret: true, required: true },
    { key: "from", label: "From", required: true, help: "A number or an alphanumeric sender id." },
    SMS_BASE,
  ],
  messagebird: [{ key: "api_key", label: "Access key", secret: true, required: true }, { key: "from", label: "From", required: true, help: "A number or an alphanumeric sender id." }, SMS_BASE],
  bird: [
    { key: "api_key", label: "Access key", secret: true, required: true },
    { key: "workspace_id", label: "Workspace id", required: true },
    { key: "channel_id", label: "Channel id", required: true },
    SMS_BASE,
  ],
  sns: [
    { key: "access_key_id", label: "Access key id", required: true },
    { key: "secret_access_key", label: "Secret access key", secret: true, required: true },
    { key: "session_token", label: "Session token", secret: true, help: "Optional. Only for temporary credentials." },
    { key: "region", label: "Region", required: true, placeholder: "eu-west-1" },
    { key: "sender_id", label: "Sender id", help: "Optional. Shown to the recipient where the country allows it." },
    { key: "sms_type", label: "Message type", options: ["Transactional", "Promotional"], help: "Transactional is for one-time codes." },
    { key: "endpoint", label: "Endpoint", kind: "url", help: "Optional. Overrides the SNS address." },
  ],
  plivo: [
    { key: "auth_id", label: "Auth id", required: true },
    { key: "auth_token", label: "Auth token", secret: true, required: true },
    { key: "from", label: "From", required: true },
    SMS_BASE,
  ],
};

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
export const isEmail = (s: string) => EMAIL.test(s.trim());

/** An http(s) URL with a host and no credentials. */
export function isHttpUrl(s: string): boolean {
  try {
    const u = new URL(s);
    return /^https?:$/.test(u.protocol) && !!u.hostname && !u.username && !u.password;
  } catch {
    return false;
  }
}

const isSetSecret = (v: unknown) => typeof v === "string" && v.length > 0;

/** Problems with a gateway form by field key: a missing required value, a bad URL, address or port. Empty when the form can be saved. */
export function checkGateway<K extends string>(fields: GatewayField<K>[], values: Record<string, string | number | null | undefined>): Record<string, string> {
  const errors: Record<string, string> = {};
  for (const f of fields) {
    const v = values[f.key];
    const text = typeof v === "number" ? String(v) : (v ?? "");
    const set = f.secret ? isSetSecret(v) : text.trim() !== "";
    if (f.required && !set) { errors[f.key] = `${f.label} is required.`; continue; }
    if (!set || f.secret) continue;
    if (f.kind === "url" && !isHttpUrl(text)) errors[f.key] = "Use an http(s) URL without credentials.";
    else if (f.kind === "email" && !isEmail(text)) errors[f.key] = "Use an email address such as bounces@example.com.";
    else if (f.kind === "port" && !(/^\d+$/.test(text) && Number(text) >= 1 && Number(text) <= 65535)) errors[f.key] = "Use a port from 1 to 65535.";
  }
  return errors;
}

/** The mail-specific rules the server enforces on top of the field table. */
export function checkMail(provider: MailProviderType, values: Record<string, string | number | null | undefined>, fromAddress: string): Record<string, string> {
  const errors = checkGateway(MAIL_FIELDS[provider], values);
  if (fromAddress.trim() && !isEmail(fromAddress)) errors.from_address = "Use an email address such as no-reply@example.com.";
  if (provider === "smtp") {
    const user = String(values.username ?? "").trim() !== "";
    const pass = isSetSecret(values.password);
    if (user && !pass) errors.password = "A username needs a password. Set both, or neither.";
    if (!user && pass) errors.username = "A password needs a username. Set both, or neither.";
  }
  if (provider === "postmark" && String(values.message_stream ?? "").trim() && !/^[A-Za-z0-9_-]+$/.test(String(values.message_stream))) errors.message_stream = "Use letters, digits, - and _.";
  if (provider === "mailgun" && !errors.domain && String(values.domain ?? "").trim() && !/^(?=.{1,253}$)[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+$/.test(String(values.domain).trim())) errors.domain = "Use a sending domain such as mg.example.com.";
  return errors;
}

/** The request body for a mail provider form: only the fields of this provider that have a value (secrets as typed, or the mask to keep one). */
export function mailBody(provider: MailProviderType, values: Record<string, string | number | null | undefined>, fromAddress: string): MailProviderConfig {
  const body: Record<string, unknown> = { provider };
  for (const f of MAIL_FIELDS[provider]) {
    const v = values[f.key];
    if (f.secret) {
      if (typeof v === "string") body[f.key] = v;
    } else if (v !== undefined && v !== null && String(v).trim() !== "") {
      body[f.key] = f.kind === "port" ? Number(v) : String(v).trim();
    }
  }
  if (fromAddress.trim()) body.from_address = fromAddress.trim();
  return body as unknown as MailProviderConfig;
}
