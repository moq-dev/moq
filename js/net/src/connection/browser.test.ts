import { expect, test } from "bun:test";
import { isWebTransportUserAgentSupported } from "./browser.ts";

test.each([
	[
		"Safari on macOS",
		"Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15",
	],
	[
		"Safari on iOS",
		"Mozilla/5.0 (iPhone; CPU iPhone OS 26_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Mobile/15E148 Safari/604.1",
	],
	[
		"Chrome on iOS",
		"Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/126.0.6478.54 Mobile/15E148 Safari/604.1",
	],
	[
		"Firefox on iOS",
		"Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) FxiOS/127.0 Mobile/15E148 Safari/605.1.15",
	],
	[
		"Edge on iOS",
		"Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) EdgiOS/126.0.2592.56 Version/18.0 Mobile/15E148 Safari/604.1",
	],
	["Firefox 151", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:151.0) Gecko/20100101 Firefox/151.0"],
])("disables WebTransport on %s", (_browser, userAgent) => {
	expect(isWebTransportUserAgentSupported(userAgent)).toBeFalse();
});

test.each([
	[
		"Chrome",
		"Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36",
	],
	[
		"Chrome on Android",
		"Mozilla/5.0 (Linux; Android 16) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Mobile Safari/537.36",
	],
	["Firefox 153", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:153.0) Gecko/20100101 Firefox/153.0"],
])("enables WebTransport on %s", (_browser, userAgent) => {
	expect(isWebTransportUserAgentSupported(userAgent)).toBeTrue();
});
