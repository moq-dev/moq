/** The wire transport a session runs over. */
export type Transport = "webtransport" | "websocket";

/**
 * Which transport {@link connect} will attempt for a user agent. Safari and Firefox always get
 * WebSocket, whether or not they expose `WebTransport`.
 *
 * @internal Not re-exported from the package entrypoint.
 */
export function pickTransport(userAgent: string, hasWebTransport: boolean): Transport {
	const ua = userAgent.toLowerCase();

	// Firefox's WebTransport implementation drops server-initiated bidi streams,
	// breaking publish (the relay opens a subscribe bidi back to us).
	// TODO: remove once Firefox fixes incoming bidi delivery.
	if (ua.includes("firefox")) return "websocket";

	// Reading `WebTransport.datagrams.readable` tears down the whole session on Safari, and
	// moq-lite-05 always reads it. There is no catchable failure, so Safari never gets WebTransport.
	// Matches Safari-style WebKit: reports "safari" but is not Chrome or an Android WebView, which
	// also carry it. This also catches every iOS browser, since they are all WebKit.
	//
	// This rule must agree with `detectSafari` in `@moq/hang`'s `util/hacks.ts`, which the support
	// matrix uses to report the transport this function actually picks. The two cannot share code:
	// `@moq/hang` depends on `@moq/net`, and `pickTransport` is internal. The shared test corpus in
	// `transport.test.ts` and `hacks.test.ts` is what catches a drift.
	if (ua.includes("safari") && !ua.includes("chrome") && !ua.includes("android")) return "websocket";

	return hasWebTransport ? "webtransport" : "websocket";
}
