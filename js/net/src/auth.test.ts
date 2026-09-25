import { describe, expect, test } from "bun:test";
import type { Getter } from "@moq/signals";
import { type Grant, type Issued, Unsupported } from "./auth.ts";
import { accept as acceptSession, connect as connectSession, type Established } from "./connection/index.ts";
import { SessionCode, SessionError } from "./error.ts";
import * as Ietf from "./ietf/index.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair, type MockTransport } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";

const url = new URL("https://localhost:4443/test");

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

// Every case runs on moq-lite-06 and on moq-transport with the MoQ Auth extension.
describe.each([Lite.ALPN_06, Ietf.ALPN.DRAFT_17, Ietf.ALPN.DRAFT_22])("%s", (protocol) => {
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

		client.close();
		server.close();
	});

	test("a token without an acceptor reports unsupported", async () => {
		const { client, server } = await connect({ publish: new OriginProducer(), protocol });
		await waitFor(client.auth.grant, (g) => g !== undefined);
		await expect(client.auth.add("token")).rejects.toBeInstanceOf(Unsupported);
		client.close();
		server.close();
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
		server.close();
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
		const first = await announced.next();
		expect(first?.prefix).toBe(Path.from("a/x"));
		expect(first?.kind).toBe("announced");

		issued[0]?.revoke(SessionCode.Unauthorized, "expired");
		const second = await announced.next();
		expect(second?.prefix).toBe(Path.from("a/x"));
		expect(second?.kind).toBe("retracted");

		// The union is empty but still a grant, and a new token restores it.
		const empty = await waitFor(client.auth.grant, (g) => g !== undefined && g.publish.size === 0);
		expect(empty?.subscribe.size).toBe(0);
		const token = await client.auth.add("again");
		expect(token.grant.peek()?.publish.equals(patterns("a"))).toBe(true);
		const third = await announced.next();
		expect(third?.kind).toBe("announced");

		let closed = false;
		void transport.closed.then(() => {
			closed = true;
		});
		await new Promise((resolve) => setTimeout(resolve, 10));
		expect(closed).toBe(false);

		announced.close();
		client.close();
		server.close();
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
		client.close();
		server.close();
	});

	test("a grant the wire cannot carry is unsupported and leaves the rest alone", async () => {
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

		// An update the wire cannot carry revokes that token's grant, and only that one.
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
		client.close();
		server.close();
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
		client.close();
		server.close();
	});
});

test.each([Lite.ALPN_05, Ietf.ALPN.DRAFT_16])("%s has no grant", async (protocol) => {
	const { client, server } = await connect({ publish: new OriginProducer(), protocol });
	expect(client.auth.grant.peek()).toBeUndefined();
	await expect(client.auth.add("token")).rejects.toBeInstanceOf(Unsupported);
	client.close();
	server.close();
});
