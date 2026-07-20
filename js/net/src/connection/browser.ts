/** Returns whether a browser user agent has a usable WebTransport implementation. */
export function isWebTransportUserAgentSupported(userAgent: string): boolean {
	const normalized = userAgent.toLowerCase();

	// Firefox only allows two concurrent remote-initiated streams:
	// https://bugzilla.mozilla.org/show_bug.cgi?id=2046262
	if (normalized.includes("firefox")) return false;

	// Safari's flow-control window never refills, which permanently stalls sessions:
	// https://bugs.webkit.org/show_bug.cgi?id=319818
	const isSafari =
		normalized.includes("safari") &&
		!normalized.includes("chrome") &&
		!normalized.includes("chromium") &&
		!normalized.includes("android");
	return !isSafari;
}

/** Returns whether this runtime can connect with WebTransport. */
export function isWebTransportSupported(): boolean {
	if (typeof globalThis.WebTransport === "undefined") return false;
	if (typeof navigator === "undefined") return true;
	return isWebTransportUserAgentSupported(navigator.userAgent);
}
