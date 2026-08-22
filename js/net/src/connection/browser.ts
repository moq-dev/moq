import Bowser from "bowser";

/** Returns whether a browser user agent has a usable WebTransport implementation. */
export function isWebTransportUserAgentSupported(userAgent: string): boolean {
	const browser = Bowser.getParser(userAgent);
	const supported = browser.satisfies({
		// Fixed with 153.0.0, Firefox only allows two concurrent remote-initiated streams:
		// https://bugzilla.mozilla.org/show_bug.cgi?id=2046262
		firefox: ">=153.0",

		// Safari's flow-control window never refills, which permanently stalls sessions:
		// https://bugs.webkit.org/show_bug.cgi?id=319818
		safari: "<0",
	});

	if (supported === undefined) {
		//By default, other browsers are considered to support WebTransport.
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
