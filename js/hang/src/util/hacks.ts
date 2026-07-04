const ua = navigator.userAgent.toLowerCase();

/** True when running in Chrome, used to work around https://issues.chromium.org/issues/40504498. */
export const isChrome = ua.includes("chrome");

/** True when running in Firefox, used to work around https://bugzilla.mozilla.org/show_bug.cgi?id=1967793. */
export const isFirefox = ua.includes("firefox");

/**
 * True on Safari / WebKit-on-Apple. Its WebCodecs backend hardware-encodes only H.264 and HEVC
 * (VideoToolbox); VP8/VP9 fall back to software (libvpx) and AV1 encode is unsupported. Chrome and
 * Android WebView also carry "safari" in their user agent, so exclude them.
 */
export const isSafari = ua.includes("safari") && !isChrome && !ua.includes("android");
