import { expect, test } from "bun:test";
import { GrantSchema, RequestSchema } from "./contract.ts";

// The fixtures below are what the Rust `moq_auth::Request` and `Grant` serialize to,
// so a server written against these schemas reads what a relay sends.

test("a connect request parses with every field", () => {
	const request = RequestSchema.parse({
		id: "00ff",
		event: "connect",
		node: "relay-1",
		transport: "quic",
		remote: "203.0.113.9:4433",
		local: "[::1]:443",
		server_name: "relay.example",
		alpn: "moq-lite-05",
		path: "/demo/room",
		query: "jwt=abc",
		role: "publisher",
		tls: { name: "edge0", fingerprint: "ab".repeat(32), expires: 4102444800, issuer: "CN=cluster" },
	});
	expect(request.event).toBe("connect");
	expect(request.tls?.name).toBe("edge0");
});

test("an end request carries its facts beside the rest", () => {
	const request = RequestSchema.parse({
		id: "00ff",
		event: "end",
		node: "relay-1",
		transport: "unix",
		path: "",
		reason: "disconnected",
		duration: 1.5,
		bytes: { sent: 10, received: 20 },
	});
	if (request.event !== "end") throw new Error("expected an end");
	expect(request.reason).toBe("disconnected");
	expect(request.bytes.received).toBe(20);

	const invalid = RequestSchema.parse({
		id: "00ff",
		event: "end",
		node: "relay-1",
		transport: "unix",
		path: "",
		reason: "invalid",
		duration: 1.5,
		bytes: { sent: 10, received: 20 },
	});
	if (invalid.event !== "end") throw new Error("expected an end");
	expect(invalid.reason).toBe("invalid");
});

test("a SETUP token parses as the Rust vector", () => {
	// What `moq_auth::Request` serializes a CAT of bytes 00 fb ff to.
	const request = RequestSchema.parse(
		JSON.parse(
			'{"id":"00ff","event":"connect","node":"relay-1","transport":"quic","path":"/demo/room","token":{"kind":1,"value":"APv_"}}',
		),
	);
	expect(request.token).toEqual({ kind: 1, value: "APv_" });

	expect(() =>
		RequestSchema.parse({
			id: "1",
			event: "connect",
			node: "n",
			transport: "quic",
			path: "/",
			token: { kind: 0, value: "AP+/" },
		}),
	).toThrow();
});

test("a connect must not carry end facts, and an unknown transport is refused", () => {
	expect(() => RequestSchema.parse({ id: "1", event: "end", node: "n", transport: "quic", path: "/" })).toThrow();
	expect(() =>
		RequestSchema.parse({ id: "1", event: "connect", node: "n", transport: "carrier-pigeon", path: "/" }),
	).toThrow();
});

test("a grant round trips and is validated", () => {
	const grant = GrantSchema.parse({
		publish: ["alice/**"],
		subscribe: ["**"],
		root: "pid/room",
		expires: 4102444800,
		revalidate: 60,
		tier: "websocket",
	});
	expect(grant.publish).toEqual(["alice/**"]);

	expect(() => GrantSchema.parse({})).toThrow(/name something/);
	expect(() => GrantSchema.parse({ publish: ["**"], revalidate: 60 })).toThrow(/must expire/);
	expect(() => GrantSchema.parse({ publish: ["**"], expires: 4102444800, revalidate: 0 })).toThrow();
	// The Rust side reads whole seconds; a fraction would be refused there.
	expect(() => GrantSchema.parse({ publish: ["**"], expires: 4102444800, revalidate: 0.5 })).toThrow();
	expect(() => GrantSchema.parse({ publish: ["**"], expires: 4102444800.5 })).toThrow();
	expect(() => GrantSchema.parse({ publish: ["a/**/b/**"] })).toThrow();
});
