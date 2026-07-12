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

/**
 * Major Safari version from the user agent's `Version/NN` token, or undefined when absent (some
 * WebKit builds, and iOS browsers like Chrome/CriOS, omit it). Only the major number is returned.
 */
export function detectSafariVersion(ua: string): number | undefined {
	// \b so a token ending in "version" (some in-app-browser UAs) can't steal the match.
	const match = ua.toLowerCase().match(/\bversion\/(\d+)/);
	return match ? Number(match[1]) : undefined;
}

// Major iOS/iPadOS version from the `CPU iPhone OS NN` / `CPU OS NN` token. Every iOS WebKit browser
// carries it, including Chrome (CriOS) and Firefox (FxiOS) and in-app WebViews, which omit the Safari
// `Version/` token. undefined off iOS. Private: only the worker-capture gate needs it.
function detectIosVersion(ua: string): number | undefined {
	const match = ua.toLowerCase().match(/\bcpu (?:iphone )?os (\d+)/);
	return match ? Number(match[1]) : undefined;
}

/**
 * True when the WebKit behind this UA is new enough to capture video in a Worker: Safari 18 added
 * worker-scope MediaStreamTrackProcessor and transferable MediaStreamTrack. The version comes from
 * the Safari `Version/NN` token, or, for iOS WebKit browsers that omit it (Chrome/Firefox and in-app
 * WebViews on iOS are all WebKit), from the iOS OS version, which tracks the Safari WebKit major. So
 * an iOS 18 Chrome qualifies exactly like Safari 18. A UA with neither version signal does not.
 */
export function detectSafariWorkerCapture(ua: string): boolean {
	if (!detectSafari(ua)) return false;
	const version = detectSafariVersion(ua) ?? detectIosVersion(ua);
	return version !== undefined && version >= 18;
}

/** True when running in Chrome, used to work around https://issues.chromium.org/issues/40504498. */
export const isChrome = detectChrome(userAgent);

/** True when running in Firefox, used to work around https://bugzilla.mozilla.org/show_bug.cgi?id=1967793. */
export const isFirefox = detectFirefox(userAgent);

/** True for Safari-style WebKit user agents (see {@link detectSafari}). */
export const isSafari = detectSafari(userAgent);

/**
 * True when this WebKit browser can capture video in a Worker (see {@link detectSafariWorkerCapture}).
 * Covers macOS/iOS Safari 18+ and iOS Chrome/Firefox/WebViews on iOS 18+ (all WebKit). Everything
 * else, including a WebKit UA too old or with no detectable version, uses the rAF capture path.
 */
export const safariWorkerCapture = detectSafariWorkerCapture(userAgent);
