const userAgent = typeof navigator !== "undefined" ? navigator.userAgent : "";

function detectChrome(ua: string): boolean {
	return ua.toLowerCase().includes("chrome");
}

/**
 * True for Safari and the other WebKit browsers, which all carry `Safari` in the user agent.
 *
 * Chrome and the Android stock browser say `Safari` too, so both have to be excluded. iOS Chrome
 * (CriOS) and iOS Firefox (FxiOS) are WebKit under the hood and deliberately do match.
 */
export function detectSafari(ua: string): boolean {
	const lower = ua.toLowerCase();
	return lower.includes("safari") && !lower.includes("android") && !detectChrome(ua);
}

/** True when running in Chrome, used to work around https://issues.chromium.org/issues/40504498. */
export const isChrome = detectChrome(userAgent);

/** True when running in Firefox, used to work around https://bugzilla.mozilla.org/show_bug.cgi?id=1967793. */
export const isFirefox = userAgent.toLowerCase().includes("firefox");

/** True when running in Safari (see {@link detectSafari}), whose hardware encode support we can't probe. */
export const isSafari = detectSafari(userAgent);
