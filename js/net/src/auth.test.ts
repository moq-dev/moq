import { describe, expect, spyOn, test } from "bun:test";
import type { Getter } from "@moq/signals";
import { type Grant, type Issued, Unsupported } from "./auth.ts";
import { accept as acceptSession, connect as connectSession, type Established } from "./connection/index.ts";
import { SessionCode, SessionError, StreamCode, toStreamCode } from "./error.ts";
import * as Ietf from "./ietf/index.ts";
import { LiteAuthWire } from "./lite/auth.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair, type MockTransport } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Writer } from "./stream.ts";
import { Timestamp } from "./time.ts";
import { withTimeout } from "./util/timeout.ts";
import { wireOf } from "./wire.ts";

const url = new URL("https://localhost:4443/test");

/** The next route event, skipping the live marker. */
async function nextRoute<E extends { kind: string }>(announced: {
	next(): Promise<E | undefined>;
}): Promise<Exclude<E, { kind: "live" }> | undefined> {
	for (;;) {
		const event = await announced.next();
		if (event?.kind !== "live") return event as Exclude<E, { kind: "live" }> | undefined;
	}
}

function patterns(...prefixes: string[]): Path.Patterns {
	return new Path.Patterns(prefixes.map((prefix) => Path.Pattern.subtree(prefix)));
}

function grant(publish: string[], subscribe: string[]): Grant {
	return { publish: patterns(...publish), subscribe: patterns(...subscribe) };
}

async function waitFor<T>(getter: Getter<T>, ready: (value: T) => boolean): Promise<T> {
	let value = getter.peek();
	while (!ready(value)) value = await getter.changed();
	return value;
}

interface Pair {
	client: Established;
	server: Established;
	transport: MockTransport;
}

async function connect(opts: { publish?: OriginProducer; serverPublish?: OriginProducer; protocol: string }) {
	const pair = createMockTransportPair(opts.protocol);
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client, publish: opts.publish?.consume() }),
		acceptSession({ transport: pair.server, url, publish: opts.serverPublish?.consume() }),
	]);
	return { client, server, transport: pair.client } satisfies Pair;
}

// Every case runs on moq-lite-07-wip and on moq-transport with the MoQ Auth extension.
describe.each([Lite.ALPN_07_WIP, Ietf.ALPN.DRAFT_17, Ietf.ALPN.DRAFT_22])("%s", (protocol) => {
	test("both sides learn their default grant", async () => {
		const { client, server } = await connect({ publish: new OriginProducer(), protocol });

		// The server consumes anything and publishes nothing.
		const clientGrant = await waitFor(client.auth.grant, (g) => g !== undefined);
		expect(clientGrant?.publish.equals(patterns(""))).toBe(true);
		expect(clientGrant?.subscribe.size).toBe(0);

		// The client publishes, so the server may subscribe to anything.
		const serverGrant = await waitFor(server.auth.grant, (g) => g !== undefined);
		expect(serverGrant?.publish.equals(patterns(""))).toBe(true);
		expect(serverGrant?.subscribe.equals(patterns(""))).toBe(true);

		client.abort();
		server.abort();
	});

	test("a token without an acceptor reports unsupported", async () => {
		const { client, server } = await connect({ publish: new OriginProducer(), protocol });
		await waitFor(client.auth.grant, (g) => g !== undefined);
		await expect(client.auth.add("token")).rejects.toBeInstanceOf(Unsupported);
		client.abort();
		server.abort();
	});

	test("an out-of-scope broadcast closes the session and names the path", async () => {
		const origin = new OriginProducer();
		const { client, server, transport } = await connect({ publish: origin, protocol });
		const requests = server.auth.requests();
		const issued: Issued[] = [];
		void (async () => {
			for (;;) {
				const request = await requests.next();
				if (!request) break;
				issued.push(request.accept(grant(["baz"], [])));
			}
		})();

		await waitFor(client.auth.grant, (g) => g !== undefined);
		origin.createBroadcast(Path.from("baz/ok")).announce();
		origin.createBroadcast(Path.from("foo/bar")).announce();

		const info = await transport.closed;
		expect(info.closeCode).toBe(SessionCode.Unauthorized);
		expect(info.reason).toBe("unauthorized: foo/bar");
		server.abort();
	});

	test("a revoked grant withdraws its broadcasts without closing the session", async () => {
		const origin = new OriginProducer();
		const { client, server, transport } = await connect({ publish: origin, protocol });
		const requests = server.auth.requests();
		const issued: Issued[] = [];
		void (async () => {
			for (;;) {
				const request = await requests.next();
				if (!request) break;
				issued.push(request.accept(grant(["a"], [])));
			}
		})();

		await waitFor(client.auth.grant, (g) => g !== undefined);
		origin.createBroadcast(Path.from("a/x")).announce();

		const announced = server.announced();
		const first = await nextRoute(announced);
		expect(first?.prefix).toBe(Path.from("a/x"));
		expect(first?.kind).toBe("start");

		issued[0]?.revoke(SessionCode.Unauthorized, "expired");
		const second = await nextRoute(announced);
		expect(second?.prefix).toBe(Path.from("a/x"));
		expect(second?.kind).toBe("end");

		// The union is empty but still a grant, and a new token restores it.
		const empty = await waitFor(client.auth.grant, (g) => g !== undefined && g.publish.size === 0);
		expect(empty?.subscribe.size).toBe(0);
		const token = await client.auth.add("again");
		expect(token.grant.peek()?.publish.equals(patterns("a"))).toBe(true);
		const third = await nextRoute(announced);
		expect(third?.kind).toBe("start");

		let closed = false;
		void transport.closed.then(() => {
			closed = true;
		});
		await new Promise((resolve) => setTimeout(resolve, 10));
		expect(closed).toBe(false);

		announced.close();
		client.abort();
		server.abort();
	});

	test("a refused token surfaces the acceptor's code and reason", async () => {
		const { client, server } = await connect({ publish: new OriginProducer(), protocol });
		const requests = server.auth.requests();
		void (async () => {
			for (;;) {
				const request = await requests.next();
				if (!request) break;
				if (request.token.byteLength === 0) request.accept(grant([], []));
				else request.reject(SessionCode.Unauthorized, "bad signature");
			}
		})();

		const err = await client.auth.add("forged").catch((e: unknown) => e);
		expect(err).toBeInstanceOf(SessionError);
		expect((err as SessionError).code).toBe(SessionCode.Unauthorized);
		client.abort();
		server.abort();
	});

	test("closing the session drops the grant", async () => {
		const { client, server } = await connect({ publish: new OriginProducer(), protocol });
		await waitFor(client.auth.grant, (g) => g !== undefined && g.publish.size > 0);

		// Checked before any stream notices the transport closing.
		client.abort();
		const closed = client.auth.grant.peek();
		expect(closed?.publish.size).toBe(0);
		expect(closed?.subscribe.size).toBe(0);
		server.abort();
	});

	test("an acceptor that ends a grant sees its stream close", async () => {
		const { client, server } = await connect({ publish: new OriginProducer(), protocol });
		try {
			const requests = server.auth.requests();
			const request = await requests.next();
			if (!request) throw new Error("the setup token never arrived");
			const issued = request.accept(grant(["a"], []));
			await waitFor(client.auth.grant, (g) => g !== undefined && g.publish.size > 0);

			issued.close();
			// A regression leaves `closed` pending, so bound the wait rather than hang the runner.
			expect(await withTimeout(Promise.resolve(issued.closed), 1000, "the grant stream never closed")).toBeNull();
		} finally {
			client.abort();
			server.abort();
		}
	});

	test("a refused setup token grants nothing rather than everything", async () => {
		const { client, server } = await connect({ publish: new OriginProducer(), protocol });
		const requests = server.auth.requests();
		void (async () => {
			for (;;) {
				const request = await requests.next();
				if (!request) break;
				request.reject(SessionCode.Unauthorized, "bad credential");
			}
		})();

		const empty = await waitFor(client.auth.grant, (g) => g !== undefined);
		expect(empty?.publish.size).toBe(0);
		expect(empty?.subscribe.size).toBe(0);
		client.abort();
		server.abort();
	});
});

// moq-transport carries namespace prefixes, so a grant that is not a union of subtrees
// is refused there rather than widened.
describe.each([Ietf.ALPN.DRAFT_17, Ietf.ALPN.DRAFT_22])("%s", (protocol) => {
	test("a grant namespace prefixes cannot carry is unsupported and leaves the rest alone", async () => {
		const { client, server, transport } = await connect({ publish: new OriginProducer(), protocol });
		const requests = server.auth.requests();
		const issued: Issued[] = [];
		void (async () => {
			for (;;) {
				const request = await requests.next();
				if (!request) break;
				const token = new TextDecoder().decode(request.token);
				const unions: Record<string, string[]> = {
					exact: ["room/alice"],
					mixed: ["room/**", "lobby"],
					wildcard: ["room/*/cam"],
				};
				const union = unions[token];
				const granted = union
					? { publish: new Path.Patterns(union.map((p) => Path.Pattern.parse(p))), subscribe: patterns() }
					: token === "t1"
						? grant(["b"], [])
						: grant(["a"], []);
				issued.push(request.accept(granted));
			}
		})();

		await waitFor(client.auth.grant, (g) => g?.publish.equals(patterns("a")) === true);
		for (const token of ["exact", "mixed", "wildcard"]) {
			await expect(client.auth.add(token)).rejects.toBeInstanceOf(Unsupported);
		}
		expect(client.auth.grant.peek()?.publish.equals(patterns("a"))).toBe(true);

		// An update namespace prefixes cannot carry revokes that token's grant, and only that one.
		const t1 = await client.auth.add("t1");
		await waitFor(client.auth.grant, (g) => g?.publish.equals(patterns("a", "b")) === true);
		issued[issued.length - 1]?.update({
			publish: new Path.Patterns([Path.Pattern.literal("b/exact")]),
			subscribe: patterns(),
		});
		await t1.closed;
		await waitFor(client.auth.grant, (g) => g?.publish.equals(patterns("a")) === true);

		let closed = false;
		void transport.closed.then(() => {
			closed = true;
		});
		await new Promise((resolve) => setTimeout(resolve, 10));
		expect(closed).toBe(false);
		client.abort();
		server.abort();
	});
});

// moq-lite carries patterns, so every grant arrives exactly as issued.
test("lite-07 carries literal and wildcard grants exactly", async () => {
	const { client, server } = await connect({ publish: new OriginProducer(), protocol: Lite.ALPN_07_WIP });
	const requests = server.auth.requests();
	const unions: Record<string, [string[], string[]]> = {
		"": [["a/**"], []],
		exact: [["room/alice"], []],
		wildcard: [["room/*/cam"], ["**/demo.hang"]],
		mixed: [["room/**", "lobby", "cam-*.hang"], []],
		root: [[""], ["**"]],
	};
	const parse = (texts: string[]) => new Path.Patterns(texts.map((text) => Path.Pattern.parse(text)));
	const issued: Issued[] = [];
	void (async () => {
		for (;;) {
			const request = await requests.next();
			if (!request) break;
			const [publish, subscribe] = unions[new TextDecoder().decode(request.token)] ?? [[], []];
			issued.push(request.accept({ publish: parse(publish), subscribe: parse(subscribe) }));
		}
	})();

	await waitFor(client.auth.grant, (g) => g !== undefined);
	for (const token of ["exact", "wildcard", "mixed", "root"]) {
		const [publish, subscribe] = unions[token];
		const added = await client.auth.add(token);
		const got = added.grant.peek();
		expect(got?.publish.equals(parse(publish))).toBe(true);
		expect(got?.subscribe.equals(parse(subscribe))).toBe(true);
	}
	client.abort();
	server.abort();
});

test.each([Lite.ALPN_05, Lite.ALPN_06, Ietf.ALPN.DRAFT_16])("%s has no grant", async (protocol) => {
	const { client, server } = await connect({ publish: new OriginProducer(), protocol });
	expect(client.auth.grant.peek()).toBeUndefined();
	await expect(client.auth.add("token")).rejects.toBeInstanceOf(Unsupported);
	client.abort();
	server.abort();
});

// lite-07 presents the connection's credential on an Auth Stream right away; lite-06 never
// opens one.
test.each([
	[Lite.ALPN_06, false],
	[Lite.ALPN_07_WIP, true],
])("%s opens an Auth Stream: %p", async (protocol, opens) => {
	const presented = spyOn(LiteAuthWire.prototype, "present");
	try {
		const { client, server } = await connect({ publish: new OriginProducer(), protocol });
		expect(presented).toHaveBeenCalledTimes(opens ? 2 : 0);
		client.abort();
		server.abort();
	} finally {
		presented.mockRestore();
	}
});

// moq-transport has no stream code for it, so only moq-lite resets with UNAUTHORIZED.
test("a revoked grant resets its subscriptions with UNAUTHORIZED", async () => {
	const clientOrigin = new OriginProducer();
	const up = clientOrigin.createBroadcast(Path.from("up/y"));
	up.announce();
	const upTrack = up.createTrack("video");

	const serverOrigin = new OriginProducer();
	const down = serverOrigin.createBroadcast(Path.from("room/x"));
	down.announce();
	const downTrack = down.createTrack("video");

	const { client, server } = await connect({
		publish: clientOrigin,
		serverPublish: serverOrigin,
		protocol: Lite.ALPN_07_WIP,
	});
	const requests = server.auth.requests();
	const issued: Issued[] = [];
	void (async () => {
		for (;;) {
			const request = await requests.next();
			if (!request) break;
			issued.push(request.accept(grant(["up"], ["room"])));
		}
	})();
	await waitFor(client.auth.grant, (g) => g !== undefined && g.subscribe.size > 0);

	const frame = { payload: new Uint8Array([1]), timestamp: Timestamp.fromMillis(0) };
	downTrack.appendGroup().writeFrame(frame);
	upTrack.appendGroup().writeFrame(frame);

	// The client subscribes to the server, and the server to the client.
	const downSub = wireOf(client).consume(Path.from("room/x")).track("video").subscribe().ordered();
	const upSub = wireOf(server).consume(Path.from("up/y")).track("video").subscribe().ordered();
	expect(await downSub.nextGroup()).toBeDefined();
	expect(await upSub.nextGroup()).toBeDefined();

	const resets = spyOn(Writer.prototype, "reset");
	try {
		issued[0]?.revoke(SessionCode.Unauthorized, "expired");
		const drained = async (track: typeof downSub) => {
			for (;;) if (!(await track.nextGroup())) break;
		};
		await Promise.all([drained(downSub).catch(() => void 0), drained(upSub).catch(() => void 0)]);

		const reasons = resets.mock.calls.map(([reason]) => reason);
		// Both sides of the revocation: the subscription the client cancels, and the one it
		// stops serving. Nothing reads as the session closing.
		const messages = reasons.map((reason) => (reason as Error).message);
		expect(messages).toContain("unauthorized: room/x");
		expect(messages).toContain("unauthorized: up/y");
		for (const reason of reasons) {
			expect([StreamCode.Unauthorized, StreamCode.Cancel]).toContain(toStreamCode(reason));
		}
	} finally {
		resets.mockRestore();
		downSub.close();
		upSub.close();
		client.abort();
		server.abort();
	}
});
