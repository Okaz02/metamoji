/**
 * The `@metamoji/sdk` client, as this app uses it.
 *
 * One client for the whole process, built lazily and kept in step with the
 * session Rust holds: the two sides share a cookie jar (see `transport.ts`),
 * so they must also agree on *which* tenant they are talking to and who they
 * are — the SDK stamps the account on classroom requests, and a client that
 * thinks it is signed out sends an empty one.
 *
 * `refreshClient` is called after anything that changes the session. It is not
 * automatic: the session lives in Rust, and only the caller knows when it has
 * moved.
 */

import { Metamoji } from "@metamoji/sdk";

import type { CloudSession } from "../ipc/api";
import { createTauriTransport } from "./transport";

let client: Metamoji | null = null;

/** Builds the client, or returns the one already built. */
export function api(): Metamoji {
  client ??= new Metamoji({
    transport: createTauriTransport(),
    // The jar is Rust's. Two of them would be two sessions.
    cookies: false,
    locale: locale(),
  });
  return client;
}

/**
 * Points the client at the tenant and account Rust is signed in to.
 *
 * Pass `null` on sign-out: a stale rest host would send the next call to the
 * previous school.
 */
export function adoptSession(session: CloudSession | null): void {
  const metamoji = api();
  if (!session) {
    metamoji.configure({ restHost: undefined });
    metamoji.setSession({});
    return;
  }
  metamoji.configure({ restHost: withTrailingSlash(session.restHost) });
  metamoji.setSession({
    userId: session.userId,
    loginName: session.loginName,
    // The classroom subsystem identifies an account by this, not by the id.
    email: session.email ?? session.loginName,
    coLoginId: session.coLoginId,
    companyId: session.companyId ?? undefined,
    companyName: session.companyName ?? undefined,
  });
}

function withTrailingSlash(url: string): string {
  return url.endsWith("/") ? url : `${url}/`;
}

function locale(): string {
  const tag = typeof navigator === "undefined" ? "ja-JP" : navigator.language;
  return tag.replace("-", "_");
}
