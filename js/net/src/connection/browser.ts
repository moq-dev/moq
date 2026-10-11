/** The browser versions needed by transport capability gates. @internal */
export function parseUserAgent(userAgent: string) {
	const ios = /(?:iPhone|iPad|iPod)/.test(userAgent) || /Macintosh.*Mobile\//.test(userAgent);
	const chromium = /(?:Chrome|Chromium|Edg|OPR)\//.test(userAgent);
	return {
		webkit: /AppleWebKit\//.test(userAgent) && !chromium,
		ios: ios
			? (/(?:CPU (?:iPhone )?OS|iPhone OS) ([\d_]+)/.exec(userAgent)?.[1]?.replaceAll("_", ".") ?? "unknown")
			: undefined,
		safari:
			/Safari\//.test(userAgent) && !chromium && !/(?:CriOS|FxiOS|EdgiOS)\//.test(userAgent)
				? /Version\/([\d.]+)/.exec(userAgent)?.[1]
				: undefined,
		firefox: /Firefox\/(\d+)/.exec(userAgent)?.[1],
	};
}

/** Returns whether a browser user agent has a usable WebTransport implementation. */
export function isWebTransportUserAgentSupported(userAgent: string): boolean {
	const browser = parseUserAgent(userAgent);
	// WebKit stalls long sessions (bugs.webkit.org/show_bug.cgi?id=319818).
	// iOS browsers use WebKit too, including iPadOS Safari's desktop user agent.
	if (browser.webkit || browser.ios) return false;
	// Firefox fixed its two concurrent incoming stream limit in version 153.
	return browser.firefox === undefined || Number(browser.firefox) >= 153;
}

/** Returns whether this runtime can connect with WebTransport. */
export function isWebTransportSupported(): boolean {
	if (typeof globalThis.WebTransport === "undefined") return false;
	if (typeof navigator === "undefined") return true;
	return isWebTransportUserAgentSupported(navigator.userAgent);
}
