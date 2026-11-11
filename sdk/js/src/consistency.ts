/**
 * Read-your-writes with read replicas (ADR-0029 §2.6).
 *
 * The server answers a write with `X-ForgeBase-Commit: <set>/<lsn>[, …]`.
 * The SDK keeps the highest position per set and sends them back as
 * `X-ForgeBase-Read-After` on later requests, so a replica serves a read
 * only once it has replayed your writes (else the server reads the
 * primary). `consistency: "strong"` sends `X-ForgeBase-Consistency: strong`
 * and the server reads the primary. Servers without replicas send no
 * tokens, and the SDK then sends nothing extra.
 */

/** `"session"` (default): read your own writes. `"strong"`: every read on the primary. */
export type Consistency = "session" | "strong";

export const COMMIT_HEADER = "X-ForgeBase-Commit";
export const READ_AFTER_HEADER = "X-ForgeBase-Read-After";
export const CONSISTENCY_HEADER = "X-ForgeBase-Consistency";

/** Most sets tracked (the server routes up to 8 191 sets; a client sees a few). */
const MAX_SETS = 64;
const SET_ID = /^[A-Za-z0-9_.-]{1,64}$/;
const HEX = /^[0-9A-Fa-f]{1,8}$/;

function parseToken(token: string): { set: string; lsn: bigint } | undefined {
  const t = token.trim();
  const slash = t.indexOf("/");
  if (slash <= 0) return undefined;
  const set = t.slice(0, slash);
  const parts = t.slice(slash + 1).split("/");
  if (!SET_ID.test(set) || parts.length !== 2 || !HEX.test(parts[0]!) || !HEX.test(parts[1]!)) return undefined;
  return { set, lsn: (BigInt(`0x${parts[0]}`) << 32n) | BigInt(`0x${parts[1]}`) };
}

const formatLsn = (lsn: bigint): string =>
  `${(lsn >> 32n).toString(16).toUpperCase()}/${(lsn & 0xffffffffn).toString(16).toUpperCase()}`;

/** The highest commit position per set this client has seen. */
export class CommitTokens {
  private readonly bySet = new Map<string, bigint>();

  /** Takes the tokens of a header value (`control/16/B374D848, s1/0/9`); malformed ones are ignored. */
  note(header: string | null | undefined): void {
    if (!header) return;
    for (const part of header.split(",")) {
      const t = parseToken(part);
      if (!t) continue;
      const held = this.bySet.get(t.set);
      if (held === undefined && this.bySet.size >= MAX_SETS) continue;
      if (held === undefined || t.lsn > held) this.bySet.set(t.set, t.lsn);
    }
  }

  /** The position held for one set (`16/B374D848`). */
  get(set: string): string | undefined {
    const lsn = this.bySet.get(set);
    return lsn === undefined ? undefined : formatLsn(lsn);
  }

  /** The `X-ForgeBase-Read-After` value, or `undefined` when nothing was written yet. */
  header(): string | undefined {
    if (this.bySet.size === 0) return undefined;
    return [...this.bySet.entries()]
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
      .map(([set, lsn]) => `${set}/${formatLsn(lsn)}`)
      .join(", ");
  }

  /** Forgets every token (after signing out, for example). */
  clear(): void {
    this.bySet.clear();
  }
}
