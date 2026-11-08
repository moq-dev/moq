import { expect, test } from "bun:test";
import { Milli } from "@moq/clock";
import { RefusedRedirect } from "../error.ts";
import { dialed, handover, pinnedTransport, type Redirect, target } from "./goaway.ts";

const current = new URL("https://relay.example/room?jwt=secret");

test("no URI, or a policy that ignores it, keeps the current URL", () => {
	expect(target("follow", "", current, false)).toBeUndefined();
	expect(target("same-host", "", current, false)).toBeUndefined();
	expect(target("ignore", "https://other.example/", current, false)).toBeUndefined();
});

test("an explicit URI the policy will not follow is refused, not ignored", () => {
	const refused: [Redirect, string][] = [
		["same-host", "https://other.example/"],
		["follow", "not a url"],
		["ignore", "not a url"],
		["follow", "http://relay.example/"],
		["follow", "unix:///tmp/moq.sock"],
		["follow", "https://127.0.0.1/"],
		["follow", "https://[::ffff:127.0.0.1]/"],
		["follow", "https://[::ffff:7f00:1]/"],
		["follow", "moqt://169.254.169.254/"],
		// Raw QUIC dials over https, which reads each of these as 127.0.0.1.
		["follow", "moql://127.1/"],
		["follow", "moql://0x7f000001/"],
		["follow", "moql://2130706433/"],
		["follow", "moqt://[::FFFF:127.0.0.1]/"],
	];
	for (const [policy, uri] of refused) {
		expect(() => target(policy, uri, current, false), `${policy}: ${uri}`).toThrow(RefusedRedirect);
	}
});

test("a refusal never repeats the URI, which can carry credentials", () => {
	try {
		target("same-host", "https://other.example/?jwt=leaked", current, false);
		throw new Error("not refused");
	} catch (err) {
		expect(err).toBeInstanceOf(RefusedRedirect);
		expect(String(err)).not.toContain("leaked");
	}
});

test("same-host follows a port or scheme move on the host already dialed", () => {
	expect(target("same-host", "https://relay.example:5443/", current, false)?.port).toBe("5443");
	// An explicit assignment is still one when it names the current URL.
	expect(target("same-host", current.href, current, false)?.href).toBe(current.href);
	// An upgrade is fine; only a downgrade is refused.
	const plain = new URL("http://relay.example/");
	expect(target("same-host", "https://relay.example/", plain, false)?.protocol).toBe("https:");
});

test("follow lets the peer name another public host", () => {
	expect(target("follow", "https://other.example/next", current, false)?.href).toBe("https://other.example/next");
});

test("a local endpoint may redirect to another local one", () => {
	const local = new URL("https://localhost:4443/");
	expect(target("follow", "https://127.0.0.1:9999/", local, false)?.port).toBe("9999");
});

test("a certificate pin holds the host even under follow", () => {
	expect(() => target("follow", "https://other.example/", current, true)).toThrow(RefusedRedirect);
	expect(target("follow", "https://relay.example:5443/", current, true)?.port).toBe("5443");
});

test("same-host compares the host a raw QUIC URL dials", () => {
	const raw = new URL("moql://relay.example/room");
	expect(target("same-host", "moql://RELAY.example:4443/room", raw, false)?.port).toBe("4443");
	expect(target("same-host", "https://relay.example/room", raw, false)?.protocol).toBe("https:");
	expect(() => target("same-host", "moql://other.example/", raw, false)).toThrow(RefusedRedirect);
});

test("a WebSocket fallback that connected is the host a redirect is judged against", () => {
	const primary = new URL("https://relay.example/");
	const socket = new URL("wss://edge.example/moq");
	expect(dialed(primary, "websocket", socket).href).toBe(socket.href);
	expect(dialed(primary, "webtransport", socket).href).toBe(primary.href);
	expect(dialed(primary, "websocket").href).toBe(primary.href);
});

test("a certificate pin holds only the WebTransport session that used it", () => {
	expect(pinnedTransport("webtransport", true)).toBe(true);
	expect(pinnedTransport("websocket", true)).toBe(false);
	expect(pinnedTransport("webtransport", false)).toBe(false);
});

test("the handover is the cap, lowered only by a positive peer deadline", () => {
	const cap = Milli(10_000);
	expect(handover(cap)).toBe(cap);
	expect(handover(cap, Milli(0))).toBe(cap);
	expect(handover(cap, Milli(3_000))).toBe(Milli(3_000));
	expect(handover(cap, Milli(3_600_000))).toBe(cap);
});
