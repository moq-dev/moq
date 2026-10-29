/** WebTransport over WebSockets with QMux framing: a browser polyfill and a server for an accepted socket. @module */
import type { Version } from "./session.ts";
import Session from "./session.ts";

export { SessionError, StreamError } from "./error.ts";
export type { AcceptOptions, Config, SendStreamOptions, SessionOptions, Version } from "./session.ts";
export { selectSubprotocol } from "./session.ts";
export type {
	WebSocketLike,
	WebSocketStreamCloseEvent,
	WebSocketStreamCloseInfo,
	WebSocketStreamData,
	WebSocketStreamLike,
	WebSocketStreamOpenEvent,
} from "./web-socket-stream.ts";

/** Options forwarded to every `Session` constructed by {@link install}. */
export interface InstallOptions {
	/** Wire-format version(s) by ALPN. See {@link SessionOptions.versions}. */
	versions?: Record<string, Version | Version[] | null>;
	/** Require the peer to negotiate an app protocol. See {@link SessionOptions.requireProtocol}. */
	requireProtocol?: boolean;
}

/** Install `Session` as the global `WebTransport` if the platform doesn't ship one.
 *
 * The `versions` map and `requireProtocol` flag are forwarded to every
 * `Session` constructed via the polyfill. Callers that pass only explicit
 * `{qmux-VV}.{alpn}` pairs in their `protocols` list can omit the map.
 *
 * Returns `true` if the polyfill was installed, `false` if `globalThis.WebTransport`
 * already existed.
 */
export function install(options?: InstallOptions): boolean {
	if ("WebTransport" in globalThis) return false;
	const versions = options?.versions;
	const requireProtocol = options?.requireProtocol;
	// biome-ignore lint/suspicious/noExplicitAny: polyfill — extending Session to match the WebTransport constructor signature
	(globalThis as any).WebTransport = class extends Session {
		constructor(url: string | URL, options?: WebTransportOptions) {
			super(url, { ...options, versions, requireProtocol });
		}
	};
	return true;
}

export default Session;
