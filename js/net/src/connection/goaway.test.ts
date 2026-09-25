import { expect, test } from "bun:test";
import { RefusedRedirect } from "../error.ts";
import * as Time from "../time.ts";
import { handover, isLocal, type Redirect, target } from "./goaway.ts";

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
		["follow", "http://relay.example/"],
		["follow", "unix:///tmp/moq.sock"],
		["follow", "https://127.0.0.1/"],
		["follow", "https://[::ffff:127.0.0.1]/"],
		["follow", "https://[::ffff:7f00:1]/"],
		["follow", "moqt://169.254.169.254/"],
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

test("local literals are recognized in every spelling", () => {
	const local = [
		"https://127.0.0.1/",
		"https://localhost/",
		"https://a.localhost/",
		"https://[::1]/",
		"https://[::]/",
		"https://10.0.0.1/",
		"https://172.16.0.1/",
		"https://192.168.1.1/",
		"https://169.254.1.1/",
		"https://0.0.0.0/",
		"https://[::ffff:10.0.0.1]/",
		"https://[fe80::1]/",
		"https://[fc00::1]/",
		"moqt://127.0.0.1/",
		"moqt://[::ffff:127.0.0.1]/",
		"unix:///tmp/moq.sock",
	];
	for (const url of local) expect(isLocal(new URL(url)), url).toBe(true);

	for (const url of ["https://example.com/", "https://8.8.8.8/", "https://172.32.0.1/", "https://[2606:4700::1]/"]) {
		expect(isLocal(new URL(url)), url).toBe(false);
	}
});

test("the handover is the cap, lowered only by a positive peer deadline", () => {
	const cap = Time.Milli(10_000);
	expect(handover(cap)).toBe(cap);
	expect(handover(cap, Time.Milli(0))).toBe(cap);
	expect(handover(cap, Time.Milli(3_000))).toBe(Time.Milli(3_000));
	expect(handover(cap, Time.Milli(3_600_000))).toBe(cap);
});
