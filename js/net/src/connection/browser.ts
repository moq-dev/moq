import Bowser from "bowser";

/** Returns whether a browser user agent has a usable WebTransport implementation. */
export function isWebTransportUserAgentSupported(userAgent: string): boolean {
	const browser = Bowser.getParser(userAgent);

	// Every WebKit engine stalls long sessions: the flow-control window never
	// refills (https://bugs.webkit.org/show_bug.cgi?id=319818), and incoming
	// unidirectional streams stop for good after roughly 7,600 of them or 16 MiB
	// on one session (moq-dev/moq#2388); one stream per group reaches that in
	// about two minutes. Chrome, Firefox and Edge on iOS are WebKit under the
	// hood, and iPadOS Safari reports macOS, so gate on the engine and the OS.
	if (browser.getEngine().name === "WebKit" || browser.getOS().name === "iOS") return false;

	const supported = browser.satisfies({
		// Fixed with 153.0.0, Firefox only allows two concurrent remote-initiated streams:
		// https://bugzilla.mozilla.org/show_bug.cgi?id=2046262
		firefox: ">=153.0",
	});

	if (supported === undefined) {
		// By default, other browsers are considered to support WebTransport.
		return true;
	}
	return supported;
}

/** Returns whether this runtime can connect with WebTransport. */
export function isWebTransportSupported(): boolean {
	if (typeof globalThis.WebTransport === "undefined") return false;
	if (typeof navigator === "undefined") return true;
	return isWebTransportUserAgentSupported(navigator.userAgent);
}
