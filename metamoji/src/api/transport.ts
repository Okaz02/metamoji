/**
 * The transport `@metamoji/sdk` runs on inside this app.
 *
 * Not the webview's own `fetch`. Two reasons, and both are load-bearing:
 *
 * * **Origin.** The server sends no CORS headers, and a page served from
 *   `tauri://` may not read a response from `mps.metamoji.com`.
 * * **The session.** The session is a cookie, and it is one session. Rust
 *   still makes the calls a webview cannot — the drive service's binary
 *   payloads, the classroom relay — so a jar up here would be a second login
 *   drifting away from the first.
 *
 * Every request therefore goes back down to `metamoji_fetch`, which sends it
 * through the same client, the same jar and the same `session.json` that the
 * Rust side uses.
 */

import { invoke } from "@tauri-apps/api/core";
import type { Transport, TransportRequest, TransportResponse } from "@metamoji/sdk";

interface FetchRequest {
  url: string;
  method: string;
  headers: Record<string, string>;
  /** Base64: a body may be a zip, and JSON has no bytes. */
  body: string | null;
}

interface FetchResponse {
  status: number;
  statusText: string;
  headers: Record<string, string>;
  setCookie: string[];
  body: string;
}

function toBase64(body: TransportRequest["body"]): string | null {
  if (body === undefined || body === null) return null;
  const bytes = typeof body === "string" ? new TextEncoder().encode(body) : body;
  let binary = "";
  // In chunks: `apply` on a whole document blows the argument limit.
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}

function fromBase64(body: string): Uint8Array {
  const binary = atob(body);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

/** A `Transport` that runs through Rust. */
export function createTauriTransport(): Transport {
  return async (request): Promise<TransportResponse> => {
    const response = await invoke<FetchResponse>("metamoji_fetch", {
      request: {
        url: request.url,
        method: request.method,
        headers: request.headers ?? {},
        body: toBase64(request.body),
      } satisfies FetchRequest,
    });

    return {
      status: response.status,
      statusText: response.statusText,
      headers: response.headers,
      setCookie: response.setCookie,
      body: fromBase64(response.body),
    };
  };
}
