import { describe, expect, test } from "bun:test";
import { type Grant, GrantSchema, RequestSchema } from "./contract.ts";

// The relay contract as the Rust moq-auth crate serializes it. Tokens, keys, and key
// sets run through the shared corpus instead; see corpus.test.ts.

describe("Rust-serialized contract", () => {
	// The exact bytes `moq_auth::Request` and `Grant` serialize to; see
	// request::tests::end_serializes_to_the_cross_language_vector and
	// grant::tests::serializes_to_the_cross_language_vector in the Rust crate.
	const RUST_REQUEST =
		'{"id":"00ff","event":"end","reason":"expired","duration":1.5,"bytes":{"sent":10,"received":20},"node":"relay-1","relay":"moq-relay/0.22.0","transport":"websocket","remote":"203.0.113.9:4433","path":"/demo/room","query":"jwt=abc","implementation":"moq-net-js/0.4.2"}';
	const RUST_INVALID_END =
		'{"id":"00ff","event":"end","reason":"invalid","duration":1.5,"bytes":{"sent":10,"received":20},"node":"relay-1","relay":"moq-relay/0.22.0","transport":"websocket","remote":"203.0.113.9:4433","path":"/demo/room","query":"jwt=abc","implementation":"moq-net-js/0.4.2"}';
	const RUST_GRANT =
		'{"publish":{"path":["alice/**"],"rate":8000000},"subscribe":["**"],"root":"pid/room","expires":4102444800,"revalidate":60,"tier":"websocket","label":"project/key/123","link":"upstream","peer":true}';

	test("parse a request a Rust relay sent", () => {
		const request = RequestSchema.parse(JSON.parse(RUST_REQUEST));
		expect(request.transport).toBe("websocket");
		if (request.event !== "end") throw new Error("expected an end");
		expect(request.reason).toBe("expired");
		expect(request.duration).toBe(1.5);
		expect(request.bytes).toEqual({ sent: 10, received: 20 });
		expect(request.implementation).toBe("moq-net-js/0.4.2");
		expect(request.relay).toBe("moq-relay/0.22.0");
	});

	test("parse an invalid end reason a Rust relay sent", () => {
		const request = RequestSchema.parse(JSON.parse(RUST_INVALID_END));
		if (request.event !== "end") throw new Error("expected an end");
		expect(request.reason).toBe("invalid");
	});

	test("parse gateway transports and stable end reasons from Rust", () => {
		for (const transport of ["rtmp", "srt", "webrtc"] as const) {
			for (const reason of ["narrowed", "shutdown"]) {
				const request = RequestSchema.parse({ ...JSON.parse(RUST_REQUEST), transport, reason });
				expect(request.transport).toBe(transport);
				if (request.event !== "end") throw new Error("expected an end");
				expect(request.reason).toBe(reason);
			}
		}
	});

	test("a grant typed here is what a Rust relay reads", () => {
		const grant: Grant = {
			publish: { path: ["alice/**"], rate: 8_000_000 },
			subscribe: ["**"],
			root: "pid/room",
			expires: 4102444800,
			revalidate: 60,
			tier: "websocket",
			label: "project/key/123",
			link: "upstream",
			peer: true,
		};
		expect(JSON.stringify(GrantSchema.parse(grant))).toBe(RUST_GRANT);
	});
});
