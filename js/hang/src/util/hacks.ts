/** True when running in Chrome, used to work around https://issues.chromium.org/issues/40504498. */
export const isChrome = navigator.userAgent.toLowerCase().includes("chrome");

/** True when running in Firefox, used to work around https://bugzilla.mozilla.org/show_bug.cgi?id=1967793. */
export const isFirefox = navigator.userAgent.toLowerCase().includes("firefox");

/**
 * True when running in Safari, whose `hardwareAcceleration` hint we can't trust.
 * Chrome carries `Safari` in its user agent too, so exclude it.
 */
export const isSafari = navigator.userAgent.toLowerCase().includes("safari") && !isChrome;
