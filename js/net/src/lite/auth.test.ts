import { expect, test } from "bun:test";
import type { Getter } from "@moq/signals";
import { type Grant, type Issued, Unsupported } from "../auth.ts";
import { accept as acceptSession, connect as connectSession, type Established } from "../connection/index.ts";
import { SessionCode, SessionError } from "../error.ts";
import { createMockTransportPair, type MockTransport } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { AuthError, AuthMessage, AuthOk, decodeAuthReplyMaybe, encodeAuthReply } from "./auth.ts";
import * as Lite from "./index.ts";

const url = new URL("https://localhost:4443/test");

function patterns(...prefixes: string[]): Path.Patterns {
	return new Path.Patterns(prefixes.map((prefix) => Path.Pattern.subtree(prefix)));
}

function grant(publish: string[], subscribe: string[]): Grant {
	return { publish: patterns(...publish), subscribe: patterns(...subscribe) };
}

/** Round-trip bytes through a writer and back out of a reader. */
async function roundTrip(write: (w: Writer) => Promise<void>): Promise<Reader> {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream({
			write(chunk) {
				chunks.push(new Uint8Array(chunk));
			},
		}),
	);
	await write(writer);
	writer.close();
	await writer.closed;
	const size = chunks.reduce((n, c) => n + c.byteLength, 0);
	const buf = new Uint8Array(size);
	let offset = 0;
	for (const chunk of chunks) {
		buf.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return new Reader(undefined, buf);
}

test("AUTH round-trips its token", async () => {
	const r = await roundTrip((w) => new AuthMessage(new TextEncoder().encode("jwt")).encode(w, Lite.Version.DRAFT_06));
	const msg = await AuthMessage.decode(r, Lite.Version.DRAFT_06);
	expect(new TextDecoder().decode(msg.token)).toBe("jwt");
});

test("AUTH_OK and AUTH_ERROR round-trip behind their type", async () => {
	const ok = new AuthOk(patterns(""), patterns(), 60_000);
	const err = new AuthError(SessionCode.Unauthorized, "expired");
	const r = await roundTrip(async (w) => {
		await encodeAuthReply(w, ok, Lite.Version.DRAFT_06);
		await encodeAuthReply(w, err, Lite.Version.DRAFT_06);
	});
	const first = await decodeAuthReplyMaybe(r, Lite.Version.DRAFT_06);
	expect(first).toBeInstanceOf(AuthOk);
	if (!(first instanceof AuthOk)) throw new Error("unreachable");
	// The empty prefix grants everything; the empty list grants nothing.
	expect(first.publish.equals(new Path.Patterns([Path.Pattern.all()]))).toBe(true);
	expect(first.subscribe.size).toBe(0);
	expect(first.expires).toBe(60_000);

	const second = await decodeAuthReplyMaybe(r, Lite.Version.DRAFT_06);
	expect(second).toEqual(err);
	expect(await decodeAuthReplyMaybe(r, Lite.Version.DRAFT_06)).toBeUndefined();
});

test("a grant the prefix wire cannot express is refused, never widened", async () => {
	const narrow = new AuthOk(new Path.Patterns([Path.Pattern.literal("room/alice")]), patterns());
	await expect(roundTrip((w) => narrow.encode(w, Lite.Version.DRAFT_06))).rejects.toBeInstanceOf(Unsupported);
});

test("lite-05 carries no AUTH", async () => {
	await expect(
		roundTrip((w) => new AuthMessage(new Uint8Array()).encode(w, Lite.Version.DRAFT_05)),
	).rejects.toThrow();
});

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

async function connect(opts: { publish?: OriginProducer; serverPublish?: OriginProducer; protocol?: string }) {
	const pair = createMockTransportPair(opts.protocol ?? Lite.ALPN_06);
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client, publish: opts.publish?.consume() }),
		acceptSession({ transport: pair.server, url, publish: opts.serverPublish?.consume() }),
	]);
	return { client, server, transport: pair.client } satisfies Pair;
}

test("both sides learn their default grant", async () => {
	const { client, server } = await connect({ publish: new OriginProducer() });

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
	const { client, server } = await connect({ publish: new OriginProducer() });
	await waitFor(client.auth.grant, (g) => g !== undefined);
	await expect(client.auth.add("token")).rejects.toBeInstanceOf(Unsupported);
	client.close();
	server.close();
});

test("an out-of-scope broadcast closes the session and names the path", async () => {
	const origin = new OriginProducer();
	const { client, server, transport } = await connect({ publish: origin });
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
	const { client, server, transport } = await connect({ publish: origin });
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
	const { client, server } = await connect({ publish: new OriginProducer() });
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

test("older versions have no grant", async () => {
	const { client, server } = await connect({ publish: new OriginProducer(), protocol: Lite.ALPN_05 });
	expect(client.auth.grant.peek()).toBeUndefined();
	await expect(client.auth.add("token")).rejects.toBeInstanceOf(Unsupported);
	client.close();
	server.close();
});
