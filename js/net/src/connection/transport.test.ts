import { expect, test } from "bun:test";
import { pickTransport } from "./transport.ts";

// Real user agents. The WebKit ones are the whole point: Safari's WebTransport session dies the
// moment `datagrams.readable` is read, which moq-lite-05 always does.
//
// This corpus is intentionally shared with `js/hang/src/util/hacks.test.ts`, which tests the twin
// Safari rule in `detectSafari`. Adding a case here without adding it there lets the two drift.
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
// The legacy Android stock browser. It says "safari" and "android" but never "chrome", so it is the
// only case that exercises the `android` clause; every other Android agent short-circuits on "chrome".
const ANDROID_LEGACY =
	"Mozilla/5.0 (Linux; U; Android 4.4.2; en-us; SM-G900F Build/KOT49H) AppleWebKit/534.30 (KHTML, like Gecko) Version/4.0 Mobile Safari/534.30";
// WebKitGTK, e.g. GNOME Web. Not Safari, but the same engine and the same datagram bug.
const EPIPHANY =
	"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.0 Safari/605.1.15";

test("every WebKit engine gets WebSocket, even when WebTransport exists", () => {
	// Safari 26.4+ ships WebTransport, so `hasWebTransport` is true and must be ignored.
	expect(pickTransport(SAFARI, true)).toBe("websocket");
	expect(pickTransport(IOS_SAFARI, true)).toBe("websocket");

	// Chrome and Firefox on iOS are WebKit under the skin. They report CriOS/FxiOS, never
	// "chrome"/"firefox", so they must fall through to the Safari branch rather than the default.
	expect(pickTransport(IOS_CHROME, true)).toBe("websocket");
	expect(pickTransport(IOS_FIREFOX, true)).toBe("websocket");

	// Desktop WebKit that is not Safari at all.
	expect(pickTransport(EPIPHANY, true)).toBe("websocket");
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

test("Android keeps WebTransport even when the agent says only Safari", () => {
	// Guards the `android` clause on its own: this agent passes the "safari" and !"chrome" checks,
	// so dropping `!ua.includes("android")` would wrongly route it to WebSocket.
	expect(pickTransport(ANDROID_LEGACY, true)).toBe("webtransport");
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
