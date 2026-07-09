import { expect, test } from "bun:test";
import { pickTransport } from "./transport.ts";

// Real user agents. The WebKit ones are the whole point: Safari's WebTransport session dies the
// moment `datagrams.readable` is read, which moq-lite-05 always does.
const CHROME =
	"Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const EDGE = `${CHROME} Edg/120.0.0.0`;
const FIREFOX = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:121.0) Gecko/20100101 Firefox/121.0";
const SAFARI =
	"Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
const IOS_SAFARI =
	"Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1";
const IOS_CHROME =
	"Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/119.0.6045.109 Mobile/15E148 Safari/604.1";
const IOS_FIREFOX =
	"Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) FxiOS/119.0 Mobile/15E148 Safari/605.1.15";
const ANDROID_CHROME =
	"Mozilla/5.0 (Linux; Android 13) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36";
const ANDROID_WEBVIEW =
	"Mozilla/5.0 (Linux; Android 13; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/120.0.0.0 Mobile Safari/537.36";

test("every WebKit engine gets WebSocket, even when WebTransport exists", () => {
	// Safari 26.4+ ships WebTransport, so `hasWebTransport` is true and must be ignored.
	expect(pickTransport(SAFARI, true)).toBe("websocket");
	expect(pickTransport(IOS_SAFARI, true)).toBe("websocket");

	// Chrome and Firefox on iOS are WebKit under the skin. They report CriOS/FxiOS, never
	// "chrome"/"firefox", so they must fall through to the Safari branch rather than the default.
	expect(pickTransport(IOS_CHROME, true)).toBe("websocket");
	expect(pickTransport(IOS_FIREFOX, true)).toBe("websocket");
});

test("Firefox gets WebSocket", () => {
	expect(pickTransport(FIREFOX, true)).toBe("websocket");
});

test("Chromium engines keep WebTransport", () => {
	// These carry "Safari" in their user agent but are not WebKit.
	expect(pickTransport(CHROME, true)).toBe("webtransport");
	expect(pickTransport(EDGE, true)).toBe("webtransport");
	expect(pickTransport(ANDROID_CHROME, true)).toBe("webtransport");
	expect(pickTransport(ANDROID_WEBVIEW, true)).toBe("webtransport");
});

test("no WebTransport support falls back to WebSocket", () => {
	expect(pickTransport(CHROME, false)).toBe("websocket");
	expect(pickTransport(SAFARI, false)).toBe("websocket");
});

test("an absent user agent falls back to WebTransport support", () => {
	// `navigator` is undefined under SSR, some worker scopes, and bun tests.
	expect(pickTransport("", true)).toBe("webtransport");
	expect(pickTransport("", false)).toBe("websocket");
});

test("the user agent is matched case-insensitively", () => {
	expect(pickTransport(SAFARI.toUpperCase(), true)).toBe("websocket");
	expect(pickTransport(FIREFOX.toUpperCase(), true)).toBe("websocket");
	expect(pickTransport(CHROME.toUpperCase(), true)).toBe("webtransport");
});
