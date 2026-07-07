/**
 * Minimal user-agent sniffing for transport/capability workarounds. `net` sits below `hang`, so it
 * cannot import `@moq/hang/util`'s equivalents; this mirrors their logic. The pure `detect*` helpers
 * take the UA string (for testability); the `is*` constants read `navigator.userAgent` once, which is
 * empty under Node/bun so every flag is false there.
 *
 * @module
 */

const userAgent = typeof navigator !== "undefined" ? navigator.userAgent : "";

/** True when the user agent is Chrome, Chromium, or Edge (all report "chrome"). */
export function detectChrome(ua: string): boolean {
	return ua.toLowerCase().includes("chrome");
}

/** True when the user agent is Firefox. */
export function detectFirefox(ua: string): boolean {
	return ua.toLowerCase().includes("firefox");
}

/**
 * True for Safari-style WebKit user agents: they report "safari" but are not Chrome or an Android
 * WebView (both of which also carry "safari"). Also matches every iOS browser, since Chrome/Firefox
 * on iOS are all WebKit.
 */
export function detectSafari(ua: string): boolean {
	const s = ua.toLowerCase();
	return s.includes("safari") && !s.includes("android") && !detectChrome(ua);
}

/** True when this browser is Chrome/Chromium/Edge (see {@link detectChrome}). */
export const isChrome = detectChrome(userAgent);

/** True when this browser is Firefox (see {@link detectFirefox}). */
export const isFirefox = detectFirefox(userAgent);

/** True for Safari-style WebKit browsers (see {@link detectSafari}). */
export const isSafari = detectSafari(userAgent);
