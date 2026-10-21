import { describe, expect, test } from "bun:test";
import { resolveSubprotocols, selectSubprotocol } from "./session.ts";

// The bare version ALPNs the polyfill appends when `requireProtocol` is false.
const BARE = ["qmux-02", "qmux-01", "qmux-00", "webtransport"];

describe("resolveSubprotocols", () => {
	test("no protocols still offers the bare version ALPNs by default", () => {
		// Regression: a bare `new Session(url)` must advertise the wire-format
		// ALPNs so a relay can negotiate without pinning an app protocol.
		// Dropping this is what broke moq's WebSocket fallback in 0.1.x.
		expect(resolveSubprotocols([], {}, false)).toEqual(BARE);
	});

	test("requireProtocol drops the bare ALPNs", () => {
		expect(resolveSubprotocols([], {}, true)).toEqual([]);
	});

	test("a pinned app protocol still gets the bare versions appended by default", () => {
		expect(resolveSubprotocols(["moq-lite-04"], { "moq-lite-04": "qmux-01" }, false)).toEqual([
			"qmux-01.moq-lite-04",
			...BARE,
		]);
	});

	test("requireProtocol advertises only the configured pairs", () => {
		expect(resolveSubprotocols(["moq-lite-04"], { "moq-lite-04": "qmux-01" }, true)).toEqual([
			"qmux-01.moq-lite-04",
		]);
	});

	test("null version expands to every qmux draft, newest first", () => {
		expect(resolveSubprotocols(["moq-lite-04"], { "moq-lite-04": null }, true)).toEqual([
			"qmux-02.moq-lite-04",
			"qmux-01.moq-lite-04",
			"qmux-00.moq-lite-04",
		]);
	});

	test("array version preserves the caller's order", () => {
		expect(resolveSubprotocols(["m"], { m: ["qmux-00", "qmux-01"] }, true)).toEqual(["qmux-00.m", "qmux-01.m"]);
	});

	test("already-prefixed pairs pass through without a versions entry", () => {
		expect(resolveSubprotocols(["qmux-00.moq-transport-17"], {}, true)).toEqual(["qmux-00.moq-transport-17"]);
	});

	test("a bare entry with no versions mapping throws", () => {
		expect(() => resolveSubprotocols(["moq-lite-04"], {}, true)).toThrow();
	});
});

describe("selectSubprotocol", () => {
	const OPTIONS = { protocols: ["moq-lite-04"], versions: { "moq-lite-04": null }, requireProtocol: true };

	test("picks the offered pair", () => {
		expect(selectSubprotocol(["qmux-01.moq-lite-04"], OPTIONS)).toBe("qmux-01.moq-lite-04");
	});

	test("parses a raw comma-separated header", () => {
		// What `req.headers.get("sec-websocket-protocol")` actually hands back.
		expect(selectSubprotocol("qmux-00.moq-lite-04, qmux-01.moq-lite-04", OPTIONS)).toBe("qmux-01.moq-lite-04");
	});

	test("our preference wins, not the client's", () => {
		// The client lists qmux-00 first; we prefer the newest draft we support.
		// Matching the client's order here would silently pin every session to the
		// oldest draft any peer still offers.
		expect(selectSubprotocol(["qmux-00.moq-lite-04", "qmux-02.moq-lite-04"], OPTIONS)).toBe("qmux-02.moq-lite-04");
	});

	test("falls back to a bare version ALPN when the client pins no app protocol", () => {
		expect(selectSubprotocol(["qmux-01"], { protocols: ["moq-lite-04"], versions: { "moq-lite-04": null } })).toBe(
			"qmux-01",
		);
	});

	test("requireProtocol refuses a client offering only bare ALPNs", () => {
		expect(selectSubprotocol(["qmux-01", "webtransport"], OPTIONS)).toBeUndefined();
	});

	test("no overlap yields undefined", () => {
		expect(selectSubprotocol(["qmux-01.something-else"], OPTIONS)).toBeUndefined();
	});

	test("an absent or empty header yields undefined", () => {
		// A client that offered nothing can't be speaking a version we chose for it.
		expect(selectSubprotocol(null, OPTIONS)).toBeUndefined();
		expect(selectSubprotocol("", OPTIONS)).toBeUndefined();
		expect(selectSubprotocol([], OPTIONS)).toBeUndefined();
		expect(selectSubprotocol(undefined, OPTIONS)).toBeUndefined();
	});

	test("a legacy client gets the webtransport wire format by default", () => {
		expect(selectSubprotocol("webtransport", {})).toBe("webtransport");
	});
});
