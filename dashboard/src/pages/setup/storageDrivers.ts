/** The remote storage drivers and the environment variables each reads its credentials from (ADR-0040). */

/** One credential the server reads from its environment, never from this form or the config file. */
export interface EnvVar { name: string; what: string; /** The registry setting whose `source` says whether it is set. */ key?: string }

/** What each remote driver needs, in the order the server tries it (ADR-0040 §4, §5, §6). */
export const DRIVER_ENV: Record<string, { label: string; intro: string; vars: EnvVar[]; fallback?: string }> = {
  s3: {
    label: "Amazon S3 and compatible",
    intro: "Access keys for the S3 API (MinIO, R2, B2, Wasabi, Spaces, or Google Cloud Storage with HMAC keys).",
    vars: [
      { name: "FORGEBASE_STORAGE_S3_ACCESS_KEY", what: "The access key id.", key: "storage.s3_access_key" },
      { name: "FORGEBASE_STORAGE_S3_SECRET_KEY", what: "The secret access key.", key: "storage.s3_secret_key" },
      { name: "FORGEBASE_STORAGE_S3_SESSION_TOKEN", what: "Optional: a temporary session token.", key: "storage.s3_session_token" },
    ],
  },
  gcs: {
    label: "Google Cloud Storage",
    intro: "Native Google Cloud Storage. The first of these that is set is used; with none, the Google Cloud metadata server (workload identity) is used.",
    vars: [
      { name: "FORGEBASE_STORAGE_GCS_CREDENTIALS_JSON", what: "The contents of a service-account JSON key.", key: "storage.gcs_credentials_json" },
      { name: "FORGEBASE_STORAGE_GCS_CREDENTIALS_FILE", what: "Or the path of that key file (GOOGLE_APPLICATION_CREDENTIALS works too)." },
      { name: "FORGEBASE_STORAGE_GCS_ACCESS_TOKEN", what: "Optional: a fixed OAuth access token, for tests and short-lived tooling.", key: "storage.gcs_access_token" },
    ],
    fallback: "With none set, ForgeBase asks the Google Cloud metadata server for a token: the right choice on GKE or Compute Engine with workload identity.",
  },
  azure: {
    label: "Azure Blob Storage",
    intro: "Azure Blob Storage. The first of these that applies is used; with none, the host's managed identity is used.",
    vars: [
      { name: "FORGEBASE_STORAGE_AZURE_ACCOUNT_KEY", what: "The storage account's shared key.", key: "storage.azure_account_key" },
      { name: "FORGEBASE_STORAGE_AZURE_SAS_TOKEN", what: "Or a shared access signature for the container.", key: "storage.azure_sas_token" },
      { name: "FORGEBASE_STORAGE_AZURE_TENANT_ID", what: "Or Microsoft Entra client credentials: the tenant id," },
      { name: "FORGEBASE_STORAGE_AZURE_CLIENT_ID", what: "the application (client) id," },
      { name: "FORGEBASE_STORAGE_AZURE_CLIENT_SECRET", what: "and the client secret.", key: "storage.azure_client_secret" },
    ],
    fallback: "With none set, ForgeBase uses the managed identity of the host (App Service, Container Apps or IMDS).",
  },
};

/** Which driver a storage setting belongs to; undefined for settings every driver shares. `s3_part_size` is shared by the three remote drivers. */
export function driverOf(key: string): "s3" | "gcs" | "azure" | undefined {
  if (key === "storage.s3_part_size") return undefined;
  if (key.startsWith("storage.s3_")) return "s3";
  if (key.startsWith("storage.gcs_")) return "gcs";
  if (key.startsWith("storage.azure_")) return "azure";
  return undefined;
}

