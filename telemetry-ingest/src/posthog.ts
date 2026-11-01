/** Optional processor for the existing validated ingest; no second collector. */
import {
  CONSENT_VERSION,
  NOTICE_VERSION,
  SCHEMA_VERSION,
  V3_NOTICE_VERSION,
  V3_SCHEMA_VERSION,
  type Batch,
} from "./schema";

export interface PostHogConfig {
  /** Unset by default. Exactly one of the two regional HTTPS capture origins. */
  POSTHOG_HOST?: string;
  /** Project token, configured as a Worker secret only after activation approval. */
  POSTHOG_PROJECT_TOKEN?: string;
  /** Operator prerequisite, never code-level proof of Cloudflare header removal. */
  POSTHOG_IP_SAFE_EGRESS_VERIFIED?: string;
}

export const POSTHOG_TIMEOUT_MS = 1_500;
const HOSTS = ["https://us.i.posthog.com", "https://eu.i.posthog.com"];

/** V2 keeps its opt-in contract; v3 identifies the disclosed opt-out policy. */
export async function deliverPostHog(batch: Batch, config: PostHogConfig): Promise<void> {
  const explicitConsent = batch.schema_version === 2 && batch.consent_version === CONSENT_VERSION;
  const disclosedPolicy =
    (batch.schema_version === SCHEMA_VERSION && batch.notice_version === NOTICE_VERSION) ||
    (batch.schema_version === V3_SCHEMA_VERSION && batch.notice_version === V3_NOTICE_VERSION);
  if (
    !(explicitConsent || disclosedPolicy) ||
    batch.events.length === 0 ||
    config.POSTHOG_IP_SAFE_EGRESS_VERIFIED !== "true" ||
    !config.POSTHOG_HOST || !HOSTS.includes(config.POSTHOG_HOST) ||
    !config.POSTHOG_PROJECT_TOKEN || !/^phc_[A-Za-z0-9_-]{1,256}$/.test(config.POSTHOG_PROJECT_TOKEN)
  ) return;

  const { install_id, sent_at, events, ...envelope } = batch;
  const body = JSON.stringify({
    api_key: config.POSTHOG_PROJECT_TOKEN,
    batch: events.map(({ event, ...properties }) => ({
      event: `codewhale_${event}`,
      timestamp: sent_at,
      properties: {
        ...envelope,
        ...properties,
        distinct_id: `codewhale:${install_id}`,
        $process_person_profile: false,
        $geoip_disable: true,
        $ip: null,
      },
    })),
  });
  try {
    // Copy no incoming headers or connection metadata. Cloudflare may still
    // add platform headers; the operator guard requires a staging receipt for
    // the actual egress path. Never follow a redirect carrying the token:
    // Workers rejects `redirect: "error"` with a TypeError, so "manual" returns
    // any 3xx unfollowed and the body (with the token) never leaves for it.
    const response = await fetch(`${config.POSTHOG_HOST}/batch/`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body,
      redirect: "manual",
      credentials: "omit",
      signal: AbortSignal.timeout(POSTHOG_TIMEOUT_MS),
    });
    // No retries, response parsing, shared queue, or log containing a token or payload.
    void response.body?.cancel().catch(() => {});
  } catch {
    // Processor failure must not change the first-party ingest result.
  }
}
