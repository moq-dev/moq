// `navigator` is absent in some worker/worklet scopes and SSR, so guard the read (empty string ->
// all flags false) rather than throwing at import time. Under bun, navigator.userAgent is
// "Bun/1.3.14", which matches no token below, so tests also see false flags.
const userAgent = typeof navigator !== "undefined" ? navigator.userAgent : "";

function detectChrome(ua: string): boolean {
	return ua.toLowerCase().includes("chrome");
}

function detectFirefox(ua: string): boolean {
	return ua.toLowerCase().includes("firefox");
}

/**
 * True for Safari-style WebKit user agents: those that report "safari" but are not Chrome or an
 * Android WebView (both of which also carry "safari"). This also matches every iOS browser, since
 * Chrome, Firefox, etc. on iOS are all WebKit.
 *
 * This rule must agree with `pickTransport` in `@moq/net`'s `connection/transport.ts`, which decides
 * the transport this predicate is used to report. The two cannot share code: `@moq/net` is a
 * dependency and `pickTransport` is internal. The shared test corpus in `hacks.test.ts` and
 * `transport.test.ts` is what catches a drift.
 */
export function detectSafari(ua: string): boolean {
	const s = ua.toLowerCase();
	return s.includes("safari") && !s.includes("android") && !detectChrome(ua);
}

/** True when running in Chrome, used to work around https://issues.chromium.org/issues/40504498. */
export const isChrome = detectChrome(userAgent);

/** True when running in Firefox, used to work around https://bugzilla.mozilla.org/show_bug.cgi?id=1967793. */
export const isFirefox = detectFirefox(userAgent);

/** True for Safari-style WebKit user agents (see {@link detectSafari}). */
export const isSafari = detectSafari(userAgent);
