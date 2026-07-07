import { describe, expect, it } from "bun:test";
import { detectChrome, detectFirefox, detectSafari } from "./agent.ts";

// Real-world user agent strings; note that Chrome and Android WebViews both carry "Safari".
const UA = {
	safariMac:
		"Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15",
	iosSafari:
		"Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1",
	chromeMac:
		"Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
	firefoxMac: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:125.0) Gecko/20100101 Firefox/125.0",
	androidChrome:
		"Mozilla/5.0 (Linux; Android 13) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Mobile Safari/537.36",
};

describe("agent", () => {
	it("detects Safari and excludes Chrome/Firefox/Android WebView", () => {
		expect(detectSafari(UA.safariMac)).toBe(true);
		expect(detectSafari(UA.iosSafari)).toBe(true);
		expect(detectSafari(UA.chromeMac)).toBe(false); // Chrome's UA also carries "Safari"
		expect(detectSafari(UA.firefoxMac)).toBe(false);
		expect(detectSafari(UA.androidChrome)).toBe(false); // Android WebView also carries "Safari"
	});

	it("detects Chrome", () => {
		expect(detectChrome(UA.chromeMac)).toBe(true);
		expect(detectChrome(UA.androidChrome)).toBe(true);
		expect(detectChrome(UA.safariMac)).toBe(false);
	});

	it("detects Firefox", () => {
		expect(detectFirefox(UA.firefoxMac)).toBe(true);
		expect(detectFirefox(UA.safariMac)).toBe(false);
	});
});
