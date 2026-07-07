import { expect, test } from "bun:test";
import { detectSafari, detectSafariVersion, detectSafariWorkerCapture } from "./hacks.ts";

const UA = {
	chrome: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
	edge: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0",
	firefox: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:121.0) Gecko/20100101 Firefox/121.0",
	androidChrome:
		"Mozilla/5.0 (Linux; Android 13; Pixel 7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36",
	androidWebview:
		"Mozilla/5.0 (Linux; Android 13; Pixel 7 wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/120.0.0.0 Mobile Safari/537.36",
	safari17:
		"Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4.1 Safari/605.1.15",
	safari18:
		"Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15",
	iosSafari:
		"Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1",
	iosSafari18:
		"Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
	iosChrome:
		"Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/120.0.6099.119 Mobile/15E148 Safari/604.1",
	iosChrome18:
		"Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/130.0.0.0 Mobile/15E148 Safari/604.1",
	ipadChrome18:
		"Mozilla/5.0 (iPad; CPU OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/130.0.0.0 Mobile/15E148 Safari/604.1",
	epiphany: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.0 Safari/605.1.15",
};

test("detectSafari matches real Safari and all iOS/WebKit browsers, excludes Chrome/Edge/Firefox/Android", () => {
	expect(detectSafari(UA.safari17)).toBe(true);
	expect(detectSafari(UA.safari18)).toBe(true);
	expect(detectSafari(UA.iosSafari)).toBe(true);
	expect(detectSafari(UA.iosChrome)).toBe(true); // "CriOS" is WebKit under the hood, not Chrome
	expect(detectSafari(UA.epiphany)).toBe(true); // WebKitGTK

	expect(detectSafari(UA.chrome)).toBe(false); // "safari" + "chrome" -> Chrome
	expect(detectSafari(UA.edge)).toBe(false);
	expect(detectSafari(UA.firefox)).toBe(false);
	expect(detectSafari(UA.androidChrome)).toBe(false); // android
	expect(detectSafari(UA.androidWebview)).toBe(false); // android + chrome
});

test("detectSafariVersion parses the major Version token, else undefined", () => {
	expect(detectSafariVersion(UA.safari17)).toBe(17);
	expect(detectSafariVersion(UA.safari18)).toBe(18);
	expect(detectSafariVersion(UA.iosSafari)).toBe(17);
	expect(detectSafariVersion(UA.epiphany)).toBe(15);
	// No "Version/" token -> undefined (real Safari always has one; iOS Chrome does not).
	expect(detectSafariVersion(UA.chrome)).toBeUndefined();
	expect(detectSafariVersion(UA.iosChrome)).toBeUndefined();
	// A word ending in "version" before the real token must not steal the match (word boundary).
	expect(detectSafariVersion("Foo/1.0 SomeVersion/9 Version/17.0 Safari/605.1.15")).toBe(17);
});

test("detectSafariWorkerCapture: WebKit >= 18 via the Version/ token or the iOS OS token", () => {
	// macOS/iOS Safari: driven by the Version/ token.
	expect(detectSafariWorkerCapture(UA.safari18)).toBe(true);
	expect(detectSafariWorkerCapture(UA.safari17)).toBe(false);
	expect(detectSafariWorkerCapture(UA.iosSafari18)).toBe(true);
	expect(detectSafariWorkerCapture(UA.iosSafari)).toBe(false); // iOS 17

	// The point: iOS Chrome/Firefox omit Version/ but carry the OS token, and iOS major tracks the
	// WebKit major, so an iOS 18 iOS-Chrome is worker-capable exactly like Safari 18.
	expect(detectSafariWorkerCapture(UA.iosChrome18)).toBe(true);
	expect(detectSafariWorkerCapture(UA.ipadChrome18)).toBe(true); // iPad form: "CPU OS 18_0"
	expect(detectSafariWorkerCapture(UA.iosChrome)).toBe(false); // iOS 17

	// Non-WebKit, or WebKit with no >= 18 signal, never qualifies.
	expect(detectSafariWorkerCapture(UA.chrome)).toBe(false);
	expect(detectSafariWorkerCapture(UA.firefox)).toBe(false);
	expect(detectSafariWorkerCapture(UA.androidWebview)).toBe(false); // has Version/4.0 but Android -> not Safari
	expect(detectSafariWorkerCapture(UA.epiphany)).toBe(false); // WebKitGTK 15, no iOS token
});
