import { describe, expect, spyOn, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import type { Getter } from "@moq/signals";
import { type Grant, type Issued, Unsupported } from "./auth.ts";
import type { AuthSession } from "./auth_session.ts";
import { accept as acceptSession, connect as connectSession, type Established } from "./connection/index.ts";
import { SessionCode, SessionError, StreamCode, StreamError, toStreamCode } from "./error.ts";
import { AuthMessage as IetfAuthMessage, IetfAuthWire } from "./ietf/auth.ts";
import * as Ietf from "./ietf/index.ts";
import * as IetfMessage from "./ietf/message.ts";
import { TrackStatusRequest } from "./ietf/track.ts";
import { Version as IetfVersion } from "./ietf/version.ts";
import { AuthMessage as LiteAuthMessage, LiteAuthWire } from "./lite/auth.ts";
import * as Lite from "./lite/index.ts";
import * as LiteMessage from "./lite/message.ts";
import { createMockTransportPair, type MockTransport } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { type Stream, Writer } from "./stream.ts";
import { abandoned } from "./util/demand.ts";
import { withTimeout } from "./util/timeout.ts";
import { nextTask } from "./util/yield.ts";
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

/** Whether a request on `path` logged the end of its authorization wait. */
function waited(logged: { mock: { calls: unknown[][] } }, path: string): boolean {
	return logged.mock.calls.some(([line]) =>
		String(line).startsWith(`request waited for authorization: path=${path} `),
	);
}

/** Yield until `done` holds, failing if it never does. */
async function settles(done: () => boolean): Promise<void> {
	for (let i = 0; i < 50 && !done(); i++) await nextTask();
	expect(done()).toBe(true);
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

	test("an oversized grant update reports Unsupported and revokes only its token", async () => {
		const { client, server } = await connect({ publish: new OriginProducer(), protocol });
		const requests = server.auth.requests();
		const issued: Issued[] = [];
		const answering = (async () => {
			for (;;) {
				const request = await requests.next();
				if (!request) break;
				issued.push(request.accept(grant([request.token.byteLength === 0 ? "a" : "b"], [])));
			}
		})();
		try {
			await waitFor(client.auth.grant, (g) => g?.publish.equals(patterns("a")) === true);
			const token = await client.auth.add("update");
			await waitFor(client.auth.grant, (g) => g?.publish.equals(patterns("a", "b")) === true);
			const publish = new Path.Patterns(
				Array.from({ length: 20 }, (_, i) => Path.Pattern.subtree(`${i}${"x".repeat(4000)}`)),
			);
			issued[issued.length - 1].update({ publish, subscribe: patterns() });
			expect(await token.closed).toBeInstanceOf(Unsupported);
			expect(token.grant.peek()).toBeUndefined();
			await waitFor(client.auth.grant, (g) => g?.publish.equals(patterns("a")) === true);
		} finally {
			client.abort();
			server.abort();
			await answering;
		}
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

	test("a broadcast withdrawn while its token waits is not refused", async () => {
		const origin = new OriginProducer();
		const { client, server, transport } = await connect({ publish: origin, protocol });
		const requests = server.auth.requests();
		const setup = await requests.next();
		if (!setup) throw new Error("missing setup token");
		const initial = setup.accept(grant(["baz"], []));
		await waitFor(client.auth.grant, (g) => g !== undefined);
		const adding = client.auth.add("new");
		try {
			const unanswered = await requests.next();
			if (!unanswered) throw new Error("missing token");
			const withdrawn = origin.createBroadcast(Path.from("foo/bar"));
			withdrawn.announce();
			await nextTask();
			withdrawn.unannounce();
			await nextTask();
			const issued = unanswered.accept(grant(["other"], []));
			await adding;
			// Still enforcing: the next uncovered broadcast is the one named.
			origin.createBroadcast(Path.from("still-bad")).announce();
			const info = await transport.closed;
			expect(info.closeCode).toBe(SessionCode.Unauthorized);
			expect(info.reason).toBe("unauthorized: still-bad");
			issued.close();
		} finally {
			initial.close();
			client.abort();
			server.abort();
			await adding.catch(() => void 0);
		}
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

	test("an explicit unsupported token uses the negotiated AUTH registry", async () => {
		const { client, server } = await connect({ publish: new OriginProducer(), protocol });
		const requests = server.auth.requests();
		const answering = (async () => {
			for (;;) {
				const request = await requests.next();
				if (!request) break;
				if (request.token.byteLength === 0) request.accept(grant([], []));
				else request.reject(SessionCode.Unsupported, "credential kind unsupported");
			}
		})();
		try {
			await expect(client.auth.add("unsupported")).rejects.toBeInstanceOf(Unsupported);
		} finally {
			client.abort();
			server.abort();
			await answering;
		}
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
		// What the session enforced is not revoked by its end.
		expect((client.auth as AuthSession).scope.peek()?.publish.size).toBeGreaterThan(0);
		server.abort();
	});

	test("a request waits for its token and survives union renewal", async () => {
		const clock = new Manual();
		const restore = clock.install();
		const logged = spyOn(console, "debug");
		const origin = new OriginProducer();
		const broadcast = origin.createBroadcast(Path.from("room/x"));
		broadcast.announce();
		const published = broadcast.createTrack("video");
		published.appendGroup().writeFrame({ payload: new Uint8Array([1]) });
		const { client, server } = await connect({ serverPublish: origin, protocol });
		const requests = server.auth.requests();
		await (client.auth as AuthSession).ready(Path.from("room/x"), "subscribe");
		const setup = await requests.next();
		if (!setup) throw new Error("missing setup token");
		const initial = setup.accept(grant([], []));
		await waitFor(client.auth.grant, (g) => g !== undefined);
		const adding = client.auth.add("first");
		try {
			const unanswered = await requests.next();
			if (!unanswered) throw new Error("missing token");
			const track = wireOf(client).consume(Path.from("room/x")).track("video").subscribe();
			await nextTask();
			expect(track.closed.peek()).toBeUndefined();
			clock.advance(Nano.fromMilli(Milli(7)));
			const firstGrant = unanswered.accept(grant([], ["room"]));
			const first = await adding;
			expect(await track.recvGroup()).toBeDefined();
			expect(
				logged.mock.calls.filter(([line]) => String(line).startsWith("request waited for authorization:")),
			).toEqual([["request waited for authorization: path=room/x waited=7ms"]]);
			const renewing = client.auth.add("renewal");
			const renewal = await requests.next();
			if (!renewal) throw new Error("missing renewal");
			broadcast
				.createTrack("audio")
				.appendGroup()
				.writeFrame({ payload: new Uint8Array([3]) });
			const covered = wireOf(client).consume(Path.from("room/x")).track("audio").subscribe();
			expect(await covered.recvGroup()).toBeDefined();
			covered.close();
			const renewedGrant = renewal.accept(grant([], ["room"]));
			const renewed = await renewing;
			first.close();
			await nextTask();
			expect(track.closed.peek()).toBeUndefined();
			published.appendGroup().writeFrame({ payload: new Uint8Array([2]) });
			expect(await track.recvGroup()).toBeDefined();
			track.close();
			renewed.close();
			firstGrant.close();
			renewedGrant.close();
		} finally {
			initial.close();
			client.abort();
			server.abort();
			await adding.catch(() => void 0);
			logged.mockRestore();
			restore();
		}
	});

	test("an abandoned request stops waiting for its token", async () => {
		const { client, server } = await connect({ protocol });
		const requests = server.auth.requests();
		const setup = await requests.next();
		if (!setup) throw new Error("missing setup token");
		const initial = setup.accept(grant([], []));
		await waitFor(client.auth.grant, (g) => g !== undefined);
		const adding = client.auth.add("first");
		try {
			const unanswered = await requests.next();
			if (!unanswered) throw new Error("missing token");
			const track = new OriginProducer().createBroadcast(Path.from("room/x")).createTrack("video");
			const reader = track.subscribe();
			const ready = (client.auth as AuthSession).ready(Path.from("room/x"), "subscribe", {
				until: () => abandoned(track.demand()),
			});
			let settled = false;
			void ready.then(() => {
				settled = true;
			});
			await nextTask();
			expect(settled).toBe(false);
			reader.close();
			expect(await ready).toBe(false);
			unanswered.accept(grant([], [])).close();
		} finally {
			initial.close();
			client.abort();
			server.abort();
			await adding.catch(() => void 0);
		}
	});

	test("a subscribe abandoned while its token waits never reaches the wire", async () => {
		const logged = spyOn(console, "debug");
		const origin = new OriginProducer();
		const broadcast = origin.createBroadcast(Path.from("room/x"));
		broadcast.announce();
		broadcast.createTrack("video");
		const { client, server, transport } = await connect({ serverPublish: origin, protocol });
		const requests = server.auth.requests();
		const setup = await requests.next();
		if (!setup) throw new Error("missing setup token");
		const initial = setup.accept(grant([], []));
		await waitFor(client.auth.grant, (g) => g !== undefined);
		const adding = client.auth.add("first");
		try {
			const unanswered = await requests.next();
			if (!unanswered) throw new Error("missing token");
			const track = wireOf(client).consume(Path.from("room/x")).track("video").subscribe();
			await nextTask();
			track.close();
			await settles(() => waited(logged, "room/x"));
			const opened = transport.sendStreams.bidi.length;
			const issued = unanswered.accept(grant([], ["room"]));
			await adding;
			for (let i = 0; i < 10; i++) await nextTask();
			expect(transport.sendStreams.bidi.length).toBe(opened);
			issued.close();
		} finally {
			initial.close();
			client.abort();
			server.abort();
			await adding.catch(() => void 0);
			logged.mockRestore();
		}
	});

	// The window between the wait seeing the reader leave and acting on it spans a few
	// microtasks, so sweep where the new reader lands.
	test.each(Array.from({ length: 17 }, (_, gap) => gap))(
		"a subscribe that regains a reader while its token waits keeps waiting (%d)",
		async (gap) => {
			const origin = new OriginProducer();
			const broadcast = origin.createBroadcast(Path.from("room/x"));
			broadcast.announce();
			const published = broadcast.createTrack("video");
			published.appendGroup().writeFrame({ payload: new Uint8Array([7]) });
			const { client, server } = await connect({ serverPublish: origin, protocol });
			const requests = server.auth.requests();
			const setup = await requests.next();
			if (!setup) throw new Error("missing setup token");
			const initial = setup.accept(grant([], []));
			await waitFor(client.auth.grant, (g) => g !== undefined);
			const adding = client.auth.add("first");
			try {
				const unanswered = await requests.next();
				if (!unanswered) throw new Error("missing token");
				const track = wireOf(client).consume(Path.from("room/x")).track("video");
				const first = track.subscribe();
				await nextTask();
				first.close();
				for (let i = 0; i < gap; i++) await Promise.resolve();
				const second = track.subscribe();
				const issued = unanswered.accept(grant([], ["room"]));
				await adding;
				expect(await second.recvGroup()).toBeDefined();
				second.close();
				issued.close();
			} finally {
				initial.close();
				client.abort();
				server.abort();
				await adding.catch(() => void 0);
			}
		},
	);

	test("a session's close is not a revocation of its subscriptions", async () => {
		const origin = new OriginProducer();
		const broadcast = origin.createBroadcast(Path.from("room/x"));
		broadcast.announce();
		broadcast
			.createTrack("video")
			.appendGroup()
			.writeFrame({ payload: new Uint8Array([1]) });

		const { client, server } = await connect({ serverPublish: origin, protocol });
		await waitFor(client.auth.grant, (g) => g !== undefined && g.subscribe.size > 0);
		const track = wireOf(client).consume(Path.from("room/x")).track("video").subscribe();
		expect(await track.recvGroup()).toBeDefined();

		// The grant drops as the session closes, which can land before the transport's close
		// reaches the subscription; force that order.
		(client.auth as AuthSession).close();
		expect(client.auth.grant.peek()?.subscribe.size).toBe(0);
		await nextTask();
		expect(track.closed.peek()).toBeUndefined();

		client.abort();
		expect(await track.closed).toBeNull();
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
			expect(
				await withTimeout(
					Promise.resolve(issued.closed),
					Nano.fromMilli(Milli(1000)),
					"the grant stream never closed",
				),
			).toBeNull();
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
test("a fetch abandoned while its token waits never reaches the wire", async () => {
	const logged = spyOn(console, "debug");
	const origin = new OriginProducer();
	const broadcast = origin.createBroadcast(Path.from("room/x"));
	broadcast.announce();
	broadcast
		.createTrack("video")
		.appendGroup()
		.writeFrame({ payload: new Uint8Array([1]) });
	const { client, server, transport } = await connect({ serverPublish: origin, protocol: Lite.ALPN_07_WIP });
	const requests = server.auth.requests();
	const setup = await requests.next();
	if (!setup) throw new Error("missing setup token");
	const initial = setup.accept(grant([], []));
	await waitFor(client.auth.grant, (g) => g !== undefined);
	const adding = client.auth.add("first");
	try {
		const unanswered = await requests.next();
		if (!unanswered) throw new Error("missing token");
		const abort = new AbortController();
		const fetched = wireOf(client)
			.consume(Path.from("room/x"))
			.track("video")
			.fetchGroup(0, { signal: abort.signal });
		await nextTask();
		abort.abort(new Error("left"));
		await expect(fetched).rejects.toThrow();
		await settles(() => waited(logged, "room/x"));
		const opened = transport.sendStreams.bidi.length;
		const issued = unanswered.accept(grant([], ["room"]));
		await adding;
		for (let i = 0; i < 10; i++) await nextTask();
		expect(transport.sendStreams.bidi.length).toBe(opened);
		issued.close();
	} finally {
		initial.close();
		client.abort();
		server.abort();
		await adding.catch(() => void 0);
		logged.mockRestore();
	}
});

// The window between the wait seeing the caller leave and acting on it spans a few
// microtasks, so sweep where the new caller lands.
test.each(Array.from({ length: 17 }, (_, gap) => gap))(
	"a fetch that regains a caller while its token waits keeps waiting (%d)",
	async (gap) => {
		const origin = new OriginProducer();
		const broadcast = origin.createBroadcast(Path.from("room/x"));
		broadcast.announce();
		broadcast
			.createTrack("video")
			.appendGroup()
			.writeFrame({ payload: new Uint8Array([7]) });
		const { client, server } = await connect({ serverPublish: origin, protocol: Lite.ALPN_07_WIP });
		const requests = server.auth.requests();
		const setup = await requests.next();
		if (!setup) throw new Error("missing setup token");
		const initial = setup.accept(grant([], []));
		await waitFor(client.auth.grant, (g) => g !== undefined);
		const adding = client.auth.add("first");
		try {
			const unanswered = await requests.next();
			if (!unanswered) throw new Error("missing token");
			const track = wireOf(client).consume(Path.from("room/x")).track("video");
			const abort = new AbortController();
			const first = track.fetchGroup(0, { signal: abort.signal });
			await nextTask();
			// The last caller leaves and another coalesces before the wait observes it.
			abort.abort(new Error("left"));
			for (let i = 0; i < gap; i++) await Promise.resolve();
			const second = track.fetchGroup(0);
			await expect(first).rejects.toThrow();
			const issued = unanswered.accept(grant([], ["room"]));
			await adding;
			const group = await second;
			expect((await group.readFrame())?.payload).toEqual(new Uint8Array([7]));
			issued.close();
		} finally {
			initial.close();
			client.abort();
			server.abort();
			await adding.catch(() => void 0);
		}
	},
);

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

// moq-transport keeps the token for the requests that name it, with no grant of its own.
test.each([
	[Lite.ALPN_05, false],
	[Lite.ALPN_06, false],
	[Ietf.ALPN.DRAFT_16, true],
])("%s has no grant, keeping a token for requests: %p", async (protocol, kept) => {
	const { client, server } = await connect({ publish: new OriginProducer(), protocol });
	expect(client.auth.grant.peek()).toBeUndefined();
	if (kept) {
		const token = await client.auth.add("token");
		expect(token.grant.peek()).toBeUndefined();
		expect(client.auth.grant.peek()).toBeUndefined();
		// A request carries the bytes as added, whatever the caller does with its buffer after.
		const bytes = new TextEncoder().encode("bytes");
		const copied = await client.auth.add(bytes);
		bytes.fill(0);
		expect((client.auth as AuthSession).carried(copied)).toEqual(new TextEncoder().encode("bytes"));
		// The token ends with its session, like any other.
		client.abort();
		expect(await token.closed).toBeInstanceOf(Error);
	} else {
		await expect(client.auth.add("token")).rejects.toBeInstanceOf(Unsupported);
	}
	client.abort();
	server.abort();
});

// With MoQ Auth negotiated, a reset AUTH stream fails the token rather than keeping it for
// requests: only a peer whose SETUP omitted the extension leaves tokens to requests.
test("a reset AUTH stream fails the token on moq-transport", async () => {
	const { client, server } = await connect({ publish: new OriginProducer(), protocol: Ietf.ALPN.DRAFT_22 });
	await waitFor(client.auth.grant, (g) => g !== undefined);
	const read = spyOn(IetfAuthWire.prototype, "read").mockRejectedValue(
		new StreamError(StreamCode.Cancel, { message: "reset" }),
	);
	try {
		await expect(client.auth.add("token")).rejects.toBeInstanceOf(Unsupported);
	} finally {
		read.mockRestore();
		client.abort();
		server.abort();
	}
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

	const frame = { payload: new Uint8Array([1]) };
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

test("withdrawn queued tokens are pruned before the next application decision", async () => {
	const { client, server, transport } = await connect({ publish: new OriginProducer(), protocol: Lite.ALPN_07_WIP });
	try {
		const requests = server.auth.requests();
		const setup = await requests.next();
		if (!setup) throw new Error("setup token missing");
		setup.accept(grant(["a"], []));
		const wire = new LiteAuthWire(transport, Lite.Version.DRAFT_07);
		for (let index = 0; index < 3; index++) {
			const stream = await wire.present(new TextEncoder().encode("withdrawn"));
			await stream.writer.close();
			await stream.reader.closed;
		}
		const live = client.auth.add("live");
		live.catch(() => void 0);
		const request = await requests.next();
		if (!request) throw new Error("live token missing");
		expect(new TextDecoder().decode(request.token)).toBe("live");
		request.accept(grant(["a"], []));
		(await live).close();
		await request.closed;
	} finally {
		client.abort();
		server.abort();
	}
});

// Capture the acceptor's actual AUTH writer after a valid setup grant. The following
// bytes travel through the same connection dispatcher and presenter as a real peer.
describe.each([Lite.ALPN_07_WIP, Ietf.ALPN.DRAFT_17, Ietf.ALPN.DRAFT_22])("AUTH violations %s", (protocol) => {
	test("a second presenter message closes the session", async () => {
		const streams: Stream[] = [];
		const prototype = protocol === Lite.ALPN_07_WIP ? LiteAuthWire.prototype : IetfAuthWire.prototype;
		const original = prototype.present;
		const spy = spyOn(prototype, "present").mockImplementation(async function (
			this: LiteAuthWire & IetfAuthWire,
			token,
		) {
			const stream = await original.call(this, token);
			streams.push(stream);
			return stream;
		});
		let pair: Pair | undefined;
		try {
			pair = await connect({ protocol });
			await waitFor(pair.client.auth.grant, (g) => g !== undefined);
			await waitFor(pair.server.auth.grant, (g) => g !== undefined);
			await streams[0]?.writer.u53(0);
			expect(
				(await withTimeout(pair.transport.closed, Nano.fromMilli(Milli(100)), "second AUTH did not close"))
					.closeCode,
			).toBe(SessionCode.ProtocolViolation);
		} finally {
			spy.mockRestore();
			pair?.client.abort();
			pair?.server.abort();
		}
	});

	test("a malformed presenter AUTH closes the session", async () => {
		const pair = await connect({ protocol });
		await waitFor(pair.client.auth.grant, (g) => g !== undefined);
		await waitFor(pair.server.auth.grant, (g) => g !== undefined);
		const lite = protocol === Lite.ALPN_07_WIP;
		const prototype = lite ? LiteAuthMessage.prototype : IetfAuthMessage.prototype;
		const spy = spyOn(prototype, "encode").mockImplementation(async (writer: Writer) => {
			const body = async (w: Writer) => {
				if (!lite) await w.u62(2n);
				await w.u53(2);
			};
			if (lite) await LiteMessage.encode(writer, body);
			else await IetfMessage.encode(writer, body, IetfAuthMessage.id);
		});
		const added = pair.client.auth.add("bad").catch(() => undefined);
		try {
			expect(
				(await withTimeout(pair.transport.closed, Nano.fromMilli(Milli(100)), "malformed AUTH did not close"))
					.closeCode,
			).toBe(SessionCode.ProtocolViolation);
		} finally {
			spy.mockRestore();
			pair.client.abort();
			pair.server.abort();
			await added;
		}
	});

	for (const fault of [
		"unknown reply",
		"oversized code",
		"truncated error",
		"extra body",
		"oversized expiry",
	] as const) {
		test(`${fault} closes the session`, async () => {
			const streams: Stream[] = [];
			const lite = protocol === Lite.ALPN_07_WIP;
			const prototype = lite ? LiteAuthWire.prototype : IetfAuthWire.prototype;
			const original = prototype.grant;
			const spy = spyOn(prototype, "grant").mockImplementation(async function (
				this: LiteAuthWire & IetfAuthWire,
				stream,
				value,
			) {
				streams.push(stream);
				await original.call(this, stream, value);
			});
			let pair: Pair | undefined;
			try {
				pair = await connect({ protocol });
				await waitFor(pair.client.auth.grant, (g) => g !== undefined);
				await waitFor(pair.server.auth.grant, (g) => g !== undefined);
				const stream = streams[0];
				if (!stream) throw new Error("setup AUTH writer missing");
				if (fault === "unknown reply") {
					await stream.writer.u53(99);
				} else {
					const body = async (writer: Writer) => {
						if (fault === "oversized expiry") {
							await writer.u53(0);
							await writer.u53(0);
							await writer.u62(1n << 53n);
						} else {
							await writer.u62(fault === "oversized code" ? 1n << 32n : 1n);
							if (fault !== "truncated error") await writer.string("");
							if (fault === "extra body") await writer.u53(0);
						}
					};
					const id = fault === "oversized expiry" ? (lite ? 0 : 0x40b62) : lite ? 1 : 0x40b63;
					if (lite) await LiteMessage.encode(stream.writer, body, { id });
					else await IetfMessage.encode(stream.writer, body, id);
				}
				expect(
					(
						await withTimeout(
							pair.transport.closed,
							Nano.fromMilli(Milli(100)),
							"malformed AUTH did not close",
						)
					).closeCode,
				).toBe(SessionCode.ProtocolViolation);
			} finally {
				spy.mockRestore();
				pair?.client.abort();
				pair?.server.abort();
			}
		});
	}
});

// AUTH shares its ID space with every other request from the same peer.
describe.each([Ietf.ALPN.DRAFT_17, Ietf.ALPN.DRAFT_22])("AUTH request IDs %s", (protocol) => {
	for (const client of [true, false]) {
		for (const fault of ["reused", "wrong parity"] as const) {
			test(`${client ? "client" : "server"} ${fault} closes the session`, async () => {
				const pair = await connect({ protocol });
				await waitFor(pair.client.auth.grant, (g) => g !== undefined);
				await waitFor(pair.server.auth.grant, (g) => g !== undefined);
				const original = IetfAuthMessage.prototype.encode;
				const spy = spyOn(IetfAuthMessage.prototype, "encode").mockImplementation(async function (
					this: IetfAuthMessage,
					writer,
					version,
				) {
					this.requestId = fault === "reused" ? (client ? 0n : 1n) : client ? 1n : 0n;
					await original.call(this, writer, version);
				});
				const added = (client ? pair.client : pair.server).auth.add("bad ID").catch(() => undefined);
				try {
					expect(
						(
							await withTimeout(
								pair.transport.closed,
								Nano.fromMilli(Milli(100)),
								"invalid AUTH ID left session open",
							)
						).closeCode,
					).toBe(SessionCode.InvalidRequestId);
				} finally {
					spy.mockRestore();
					pair.client.abort();
					pair.server.abort();
					await added;
				}
			});
		}
	}
});

for (const [protocol, version] of [
	[Ietf.ALPN.DRAFT_17, IetfVersion.DRAFT_17],
	[Ietf.ALPN.DRAFT_22, IetfVersion.DRAFT_22],
] as const) {
	test(`TRACK_STATUS cannot reuse setup AUTH ID on ${protocol}`, async () => {
		const pair = await connect({ protocol });
		await waitFor(pair.client.auth.grant, (g) => g !== undefined);
		try {
			const raw = await pair.transport.createBidirectionalStream();
			const writer = new Writer(raw.writable, version);
			await writer.u53(TrackStatusRequest.id);
			await new TrackStatusRequest({
				requestId: 0n,
				trackNamespace: Path.from("test"),
				trackName: "video",
			}).encode(writer, version);
			expect(
				(
					await withTimeout(
						pair.transport.closed,
						Nano.fromMilli(Milli(100)),
						"cross-request reuse left session open",
					)
				).closeCode,
			).toBe(SessionCode.InvalidRequestId);
		} finally {
			pair.client.abort();
			pair.server.abort();
		}
	});
}

for (const protocol of [Ietf.ALPN.DRAFT_17, Ietf.ALPN.DRAFT_22]) {
	test(`AUTH accepts reordered IDs on ${protocol}`, async () => {
		const pair = await connect({ protocol });
		await waitFor(pair.client.auth.grant, (g) => g !== undefined);
		await waitFor(pair.server.auth.grant, (g) => g !== undefined);
		const original = IetfAuthMessage.prototype.encode;
		const spy = spyOn(IetfAuthMessage.prototype, "encode").mockImplementation(async function (
			this: IetfAuthMessage,
			writer,
			version,
		) {
			// Swap the next two independently opened request IDs, preserving endpoint parity.
			this.requestId = this.requestId < 4n ? this.requestId + 2n : this.requestId - 2n;
			await original.call(this, writer, version);
		});
		try {
			for (const endpoint of [pair.client, pair.server]) {
				await expect(endpoint.auth.add("first")).rejects.toBeInstanceOf(Unsupported);
				await expect(endpoint.auth.add("second")).rejects.toBeInstanceOf(Unsupported);
			}
		} finally {
			spy.mockRestore();
			pair.client.abort();
			pair.server.abort();
		}
	});
}
