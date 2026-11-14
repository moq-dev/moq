import { describe, expect, spyOn, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import {
	type Credential,
	Duplicate,
	type Grant,
	Handle,
	type Issued,
	type Request,
	type Requests,
	Unsupported,
} from "./auth.ts";
import { accept as acceptSession, connect as connectSession, type Session } from "./connection/index.ts";
import { SessionCode, SessionError, StreamCode, StreamError, toStreamCode } from "./error.ts";
import * as Ietf from "./ietf/index.ts";
import { AuthMessage as LiteAuthMessage, LiteAuthWire } from "./lite/auth.ts";
import * as Lite from "./lite/index.ts";
import * as LiteMessage from "./lite/message.ts";
import { createMockTransportPair, type MockTransport, manual, settlesWithin, waitFor } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Stream, Writer } from "./stream.ts";
import { abandoned } from "./util/demand.ts";
import { withTimeout } from "./util/timeout.ts";
import { wireOf } from "./wire.ts";

/** Wait for a token's first answer, returning it as `add` returned it. */
async function answered<T extends Credential>(token: T): Promise<T> {
	await token.answered;
	return token;
}

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

/** Whether a request on `path` logged the end of its authorization wait. */
function waited(logged: { mock: { calls: unknown[][] } }, path: string): boolean {
	return logged.mock.calls.some(([line]) =>
		String(line).startsWith(`request waited for authorization: path=${path} `),
	);
}

/** Yield until `done` holds, failing if it never does. */
async function settles(done: () => boolean): Promise<void> {
	for (let i = 0; i < 50 && !done(); i++) await yieldNow();
	expect(done()).toBe(true);
}

interface Pair {
	client: Session;
	server: Session;
	transport: MockTransport;
}

async function connect(opts: {
	publish?: OriginProducer;
	serverPublish?: OriginProducer;
	protocol: string;
	auth?: Handle;
}) {
	const pair = createMockTransportPair(opts.protocol);
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client, publish: opts.publish?.consume(), auth: opts.auth }),
		acceptSession({ transport: pair.server, url, publish: opts.serverPublish?.consume() }),
	]);
	return { client, server, transport: pair.client } satisfies Pair;
}

/** Answer every token the peer presents with `answer`'s grant, holding each until the session ends. */
function answer(requests: Requests, answer: (token: string) => Grant): Issued[] {
	const issued: Issued[] = [];
	void (async () => {
		for (;;) {
			const request = await requests.next();
			if (!request) break;
			issued.push(request.accept(answer(new TextDecoder().decode(request.token))));
		}
	})();
	return issued;
}

// Every case runs on each version with AUTH streams: moq-lite-07-wip.
describe.each([Lite.ALPN_07_WIP])("%s", (protocol) => {
	test("a shared handle presents its token on every session", async () => {
		const handle = new Handle();
		// Added before any session: it waits for the first.
		const adding = answered(handle.add("token"));

		const first = await connect({ publish: new OriginProducer(), protocol, auth: handle });
		answer(first.server.auth.requests(), (token) => (token ? grant(["first"], []) : grant([], [])));
		const token = await adding;
		await waitFor(token.grant, (g) => g?.publish.equals(patterns("first")) === true);
		expect(first.client.auth).toBe(handle);

		first.client.abort();
		first.server.abort();
		expect(token.grant.peek()).toBeUndefined();
		expect(handle.grant.peek()).toBeUndefined();
		let ended = false;
		void token.closed.then(() => {
			ended = true;
		});
		await yieldNow();
		expect(ended).toBe(false);

		const second = await connect({ publish: new OriginProducer(), protocol, auth: handle });
		answer(second.server.auth.requests(), (token) => (token ? grant(["second"], []) : grant([], [])));
		await waitFor(token.grant, (g) => g?.publish.equals(patterns("second")) === true);
		await waitFor(handle.grant, (g) => g?.publish.equals(patterns("second")) === true);
		expect(ended).toBe(false);
		// Only moq-transport requests carry a token.
		expect(wireOf(second.client).auth.carried(token)).toBeUndefined();

		token.close();
		expect(await token.closed).toBeNull();
		await waitFor(handle.grant, (g) => g?.publish.size === 0);
		second.client.abort();
		second.server.abort();
	});

	test("a shared handle presents a token whose stream is lost again on the same session", async () => {
		const clock = new Manual();
		const installed = clock.install();
		const warned = spyOn(console, "warn").mockImplementation(() => {});
		// The server's side of each AUTH stream carrying our token, in arrival order.
		const Wire = LiteAuthWire;
		const original = Wire.prototype.accept;
		const streams: Stream[] = [];
		const accepted = spyOn(Wire.prototype, "accept").mockImplementation(async function (
			this: InstanceType<typeof Wire>,
			stream: Stream,
		) {
			const token = await original.call(this, stream);
			if (new TextDecoder().decode(token) === "token") streams.push(stream);
			return token;
		});
		const handle = new Handle();
		const { client, server } = await connect({ publish: new OriginProducer(), protocol, auth: handle });
		answer(server.auth.requests(), (token) => (token ? grant(["a"], []) : grant([], [])));
		try {
			const token = await answered(handle.add("token"));
			await waitFor(token.grant, (g) => g?.publish.equals(patterns("a")) === true);
			let ended = false;
			void token.closed.then(() => {
				ended = true;
			});

			streams[0]?.abort(new StreamError(StreamCode.Internal, { message: "lost" }));
			await waitFor(token.grant, (g) => g === undefined);
			await clock.advance(Duration.millis(999));
			for (let i = 0; i < 10; i++) await yieldNow();
			expect(streams.length).toBe(1);
			await clock.advance(Duration.millis(1));
			await waitFor(token.grant, (g) => g?.publish.equals(patterns("a")) === true);
			expect(streams.length).toBe(2);
			expect(ended).toBe(false);
		} finally {
			accepted.mockRestore();
			warned.mockRestore();
			client.abort();
			server.abort();
			installed.close();
		}
	});

	test("a token whose peer resets only its sending half ends that stream before presenting again", async () => {
		const clock = new Manual();
		const installed = clock.install();
		const warned = spyOn(console, "warn").mockImplementation(() => {});
		const Wire = LiteAuthWire;
		const original = Wire.prototype.accept;
		const streams: Stream[] = [];
		const accepted = spyOn(Wire.prototype, "accept").mockImplementation(async function (
			this: InstanceType<typeof Wire>,
			stream: Stream,
		) {
			const token = await original.call(this, stream);
			if (new TextDecoder().decode(token) === "token") streams.push(stream);
			return token;
		});
		const handle = new Handle();
		const { client, server } = await connect({ publish: new OriginProducer(), protocol, auth: handle });
		answer(server.auth.requests(), (token) => (token ? grant(["a"], []) : grant([], [])));
		try {
			const token = await answered(handle.add("token"));
			await waitFor(token.grant, (g) => g?.publish.equals(patterns("a")) === true);
			const first = streams[0];
			if (!first) throw new Error("the token was never presented");
			let abandoned: unknown;
			first.reader.closed.then(
				() => {
					abandoned = "finished";
				},
				(err: unknown) => {
					abandoned = err;
				},
			);

			first.writer.reset(new StreamError(StreamCode.Internal, { message: "lost" }));
			await waitFor(token.grant, (g) => g === undefined);
			await clock.advance(Duration.millis(1000));
			await waitFor(token.grant, (g) => g?.publish.equals(patterns("a")) === true);
			expect(streams.length).toBe(2);
			expect(abandoned).toBeInstanceOf(Error);
		} finally {
			accepted.mockRestore();
			warned.mockRestore();
			client.abort();
			server.abort();
			installed.close();
		}
	});

	test("a handle backs one active session", async () => {
		const handle = new Handle();
		const first = await connect({ publish: new OriginProducer(), protocol, auth: handle });
		const pair = createMockTransportPair(protocol);
		await expect(connectSession({ url, transport: pair.client, auth: handle })).rejects.toBeInstanceOf(Duplicate);
		pair.client.close();

		first.client.abort();
		first.server.abort();
		const second = await connect({ publish: new OriginProducer(), protocol, auth: handle });
		expect(second.client.auth).toBe(handle);

		// Ending the handle ends the tokens it holds, and no session can be built on it.
		handle.abort(new Error("done"));
		expect(() => answered(handle.add("late"))).toThrow("done");
		second.client.abort();
		second.server.abort();
	});

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
		await expect(answered(client.auth.add("token"))).rejects.toBeInstanceOf(Unsupported);
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
			const token = await answered(client.auth.add("update"));
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
		origin.route(Path.from("baz/ok"), { hidden: true }).announce();
		origin.route(Path.from("foo/bar"), { hidden: true }).announce();

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
		const adding = answered(client.auth.add("new"));
		try {
			const unanswered = await requests.next();
			if (!unanswered) throw new Error("missing token");
			const withdrawn = origin.route(Path.from("foo/bar"));
			await yieldNow();
			withdrawn.withdraw();
			await yieldNow();
			const issued = unanswered.accept(grant(["other"], []));
			await adding;
			// Still enforcing: the next uncovered broadcast is the one named.
			origin.route(Path.from("still-bad"), { hidden: true }).announce();
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

	test("closing a token before its answer rejects it and withdraws it", async () => {
		// With no session live, closing withdraws it before any session presents it.
		const handle = new Handle();
		const offline = handle.add("offline");
		offline.close();
		await expect(answered(offline)).rejects.toThrow();
		expect(await offline.closed).toBeNull();

		// On a live session, closing withdraws it while the peer still holds it unanswered.
		const { client, server } = await connect({ protocol });
		const requests = server.auth.requests();
		const setup = await requests.next();
		if (!setup) throw new Error("missing setup token");
		const initial = setup.accept(grant([], []));
		try {
			const waiting = client.auth.add("waiting");
			const unanswered = await requests.next();
			if (!unanswered) throw new Error("missing token");
			waiting.close();
			await expect(answered(waiting)).rejects.toThrow();
			expect(await unanswered.closed).not.toBeUndefined();
			expect(waiting.grant.peek()).toBeUndefined();
		} finally {
			initial.close();
			client.abort();
			server.abort();
		}
	});

	test("a revoked grant withdraws its broadcasts without closing the session", async () => {
		using clock = manual();
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
		origin.route(Path.from("a/x"), { hidden: true }).announce();

		const announced = server.routes();
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
		const token = await answered(client.auth.add("again"));
		expect(token.grant.peek()?.publish.equals(patterns("a"))).toBe(true);
		const third = await nextRoute(announced);
		expect(third?.kind).toBe("start");

		expect(await settlesWithin(transport.closed, clock, Duration.millis(10))).toBe(false);

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
			await expect(answered(client.auth.add("unsupported"))).rejects.toBeInstanceOf(Unsupported);
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

		const err = await answered(client.auth.add("forged")).catch((e: unknown) => e);
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
		// No session is live, and every token ended with it.
		expect(client.auth.grant.peek()).toBeUndefined();
		const closed = wireOf(client).auth.grant.peek();
		expect(closed?.publish.size).toBe(0);
		expect(closed?.subscribe.size).toBe(0);
		// What the session enforced is not revoked by its end.
		expect(wireOf(client).auth.scope.peek()?.publish.size).toBeGreaterThan(0);
		server.abort();
	});

	test("a request waits for its token and survives union renewal", async () => {
		const clock = new Manual();
		const installed = clock.install();
		const logged = spyOn(console, "debug");
		const origin = new OriginProducer();
		const broadcastRoute = origin.route(Path.from("room/x"));
		const broadcast = broadcastRoute.broadcast;
		const published = broadcast.createTrack("video");
		published.appendGroup().writeFrame({ payload: new Uint8Array([1]) });
		const { client, server } = await connect({ serverPublish: origin, protocol });
		const requests = server.auth.requests();
		await wireOf(client).auth.ready(Path.from("room/x"), "subscribe");
		const setup = await requests.next();
		if (!setup) throw new Error("missing setup token");
		const initial = setup.accept(grant([], []));
		await waitFor(client.auth.grant, (g) => g !== undefined);
		const adding = answered(client.auth.add("first"));
		try {
			const unanswered = await requests.next();
			if (!unanswered) throw new Error("missing token");
			const track = wireOf(client).consume(Path.from("room/x")).track("video").subscribe();
			await yieldNow();
			expect(track.closed.peek()).toBeUndefined();
			clock.jump(Duration.millis(7));
			const firstGrant = unanswered.accept(grant([], ["room"]));
			const first = await adding;
			expect(await track.recvGroup()).toBeDefined();
			expect(
				logged.mock.calls.filter(([line]) => String(line).startsWith("request waited for authorization:")),
			).toEqual([["request waited for authorization: path=room/x waited=7ms"]]);
			const renewing = answered(client.auth.add("renewal"));
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
			await yieldNow();
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
			installed.close();
		}
	});

	test("an abandoned request stops waiting for its token", async () => {
		const { client, server } = await connect({ protocol });
		const requests = server.auth.requests();
		const setup = await requests.next();
		if (!setup) throw new Error("missing setup token");
		const initial = setup.accept(grant([], []));
		await waitFor(client.auth.grant, (g) => g !== undefined);
		const adding = answered(client.auth.add("first"));
		try {
			const unanswered = await requests.next();
			if (!unanswered) throw new Error("missing token");
			const track = new OriginProducer()
				.route(Path.from("room/x"), { hidden: true })
				.broadcast.createTrack("video");
			const reader = track.subscribe();
			const ready = wireOf(client).auth.ready(Path.from("room/x"), "subscribe", {
				until: () => abandoned(track.demand()),
			});
			let settled = false;
			void ready.then(() => {
				settled = true;
			});
			await yieldNow();
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
		const broadcastRoute = origin.route(Path.from("room/x"));
		const broadcast = broadcastRoute.broadcast;
		broadcast.createTrack("video");
		const { client, server, transport } = await connect({ serverPublish: origin, protocol });
		const requests = server.auth.requests();
		const setup = await requests.next();
		if (!setup) throw new Error("missing setup token");
		const initial = setup.accept(grant([], []));
		await waitFor(client.auth.grant, (g) => g !== undefined);
		const adding = answered(client.auth.add("first"));
		try {
			const unanswered = await requests.next();
			if (!unanswered) throw new Error("missing token");
			const track = wireOf(client).consume(Path.from("room/x")).track("video").subscribe();
			await yieldNow();
			track.close();
			await settles(() => waited(logged, "room/x"));
			const opened = transport.sendStreams.bidi.length;
			const issued = unanswered.accept(grant([], ["room"]));
			await adding;
			for (let i = 0; i < 10; i++) await yieldNow();
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
			const broadcastRoute = origin.route(Path.from("room/x"));
			const broadcast = broadcastRoute.broadcast;
			const published = broadcast.createTrack("video");
			published.appendGroup().writeFrame({ payload: new Uint8Array([7]) });
			const { client, server } = await connect({ serverPublish: origin, protocol });
			const requests = server.auth.requests();
			const setup = await requests.next();
			if (!setup) throw new Error("missing setup token");
			const initial = setup.accept(grant([], []));
			await waitFor(client.auth.grant, (g) => g !== undefined);
			const adding = answered(client.auth.add("first"));
			try {
				const unanswered = await requests.next();
				if (!unanswered) throw new Error("missing token");
				const track = wireOf(client).consume(Path.from("room/x")).track("video");
				const first = track.subscribe();
				await yieldNow();
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
		const broadcastRoute = origin.route(Path.from("room/x"));
		const broadcast = broadcastRoute.broadcast;
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
		wireOf(client).auth.close();
		expect(wireOf(client).auth.grant.peek()?.subscribe.size).toBe(0);
		await yieldNow();
		expect(track.closed.peek()).toBeUndefined();

		client.abort();
		const closed = await track.closed;
		expect(closed).toBeInstanceOf(SessionError);
		expect((closed as SessionError).code).toBe(SessionCode.Cancel);
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
					Duration.millis(1000),
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

// Close ends every token here unanswered, so nothing waiting on the setup token stays parked.
test("closing a session settles its unanswered setup token", async () => {
	const { client, server } = await connect({ publish: new OriginProducer(), protocol: Lite.ALPN_07_WIP });
	const requests = server.auth.requests();
	try {
		await requests.next();
		let settled = false;
		void wireOf(client)
			.auth.setupAnswered()
			.then(() => {
				settled = true;
			});
		for (let i = 0; i < 10; i++) await yieldNow();
		expect(settled).toBe(false);
		client.abort();
		await settles(() => settled);
	} finally {
		client.abort();
		server.abort();
	}
});

// moq-transport's AUTH_ERROR carries no code that may pass, so only lite retries.
test("lite-07 presents a transiently refused token again after a backoff", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const warned = spyOn(console, "warn").mockImplementation(() => {});
	const refusals = () => warned.mock.calls.filter(([line]) => line === "auth token refused").length;
	const { client, server } = await connect({ publish: new OriginProducer(), protocol: Lite.ALPN_07_WIP });
	const requests = server.auth.requests();
	const issued: Issued[] = [];
	let added = false;

	/** Refuse `request` as busy, once the client has started its backoff. */
	const refuse = async (request: Request | undefined) => {
		expect(request && new TextDecoder().decode(request.token)).toBe("token");
		const before = refusals();
		request?.reject(SessionCode.TooManyRequests, "busy");
		// The client starts its backoff as it logs the refusal.
		await settles(() => refusals() > before);
	};

	/** The token presented again, exactly once `backoff` passes. */
	const after = async (backoff: Duration) => {
		let presented = false;
		const next = requests.next().then((request) => {
			presented = true;
			return request;
		});
		await clock.advance(backoff.sub(Duration.millis(1)));
		for (let i = 0; i < 10; i++) await yieldNow();
		expect(presented).toBe(false);
		expect(added).toBe(false);
		await clock.advance(Duration.millis(1));
		return await next;
	};

	try {
		const setup = await requests.next();
		if (!setup) throw new Error("missing setup token");
		issued.push(setup.accept(grant([], [])));

		const adding = answered(client.auth.add("token"));
		void adding.then(() => {
			added = true;
		});
		await refuse(await requests.next());
		// Unanswered through the backoff, so a publication it might cover waits for the retry.
		let ready: boolean | undefined;
		void wireOf(client)
			.auth.ready(Path.from("a"), "publish")
			.then((value) => {
				ready = value;
			});
		await refuse(await after(Duration.millis(1000)));
		expect(ready).toBeUndefined();
		const accepted = await after(Duration.millis(2000));
		if (!accepted) throw new Error("the token was not presented again");
		issued.push(accepted.accept(grant(["a"], [])));
		const token = await adding;
		expect(token.grant.peek()?.publish.equals(patterns("a"))).toBe(true);
		await settles(() => ready !== undefined);
		expect(ready).toBe(true);
	} finally {
		client.abort();
		server.abort();
		warned.mockRestore();
		installed.close();
	}
});

// The peer replied to the first presentation, so it knows AUTH: a reset is a lost stream,
// presented again after the doubled backoff.
test("lite-07 presents a shared token again when its retry stream is reset", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const warned = spyOn(console, "warn").mockImplementation(() => {});
	const logged = (line: string) => warned.mock.calls.filter(([first]) => first === line).length;
	const handle = new Handle();
	const { client, server } = await connect({
		publish: new OriginProducer(),
		protocol: Lite.ALPN_07_WIP,
		auth: handle,
	});
	const requests = server.auth.requests();
	let read: ReturnType<typeof spyOn> | undefined;
	try {
		const setup = await requests.next();
		setup?.accept(grant([], []));

		let rejected: unknown;
		answered(handle.add("token")).catch((err) => {
			rejected = err;
		});
		const request = await requests.next();
		request?.reject(SessionCode.TooManyRequests, "busy");
		await settles(() => logged("auth token refused") > 0);

		read = spyOn(LiteAuthWire.prototype, "read").mockRejectedValue(
			new StreamError(StreamCode.Cancel, { message: "reset" }),
		);
		await clock.advance(Duration.millis(1000));
		await settles(() => logged("auth token ended") > 0);
		for (let i = 0; i < 10; i++) await yieldNow();
		expect(rejected).toBeUndefined();

		await clock.advance(Duration.millis(2000));
		await settles(() => logged("auth token ended") > 1);
		expect(rejected).toBeUndefined();
	} finally {
		read?.mockRestore();
		client.abort();
		server.abort();
		warned.mockRestore();
		installed.close();
	}
});

test("lite-07 presents a token added after a GOAWAY only on the successor", async () => {
	const protocol = Lite.ALPN_07_WIP;
	const handle = new Handle();
	const pair = createMockTransportPair(protocol);
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client, publish: new OriginProducer().consume(), auth: handle }),
		acceptSession({ transport: pair.server, url }),
	]);
	const seen: string[] = [];
	answer(server.auth.requests(), (token) => {
		seen.push(token);
		return token ? grant(["first"], []) : grant([], []);
	});
	const token = await answered(handle.add("token"));
	await waitFor(handle.grant, (g) => g?.publish.equals(patterns("first")) === true);

	const stream = await Stream.open(pair.server, { version: Lite.Version.DRAFT_07 });
	await stream.writer.u53(Lite.StreamId.Goaway);
	await new Lite.Goaway("").encode(stream.writer, Lite.Version.DRAFT_07);
	await settles(() => !wireOf(client).auth.active);

	const late = answered(handle.add("late"));
	const successor = await connect({ publish: new OriginProducer(), protocol, auth: handle });
	// The successor has not answered yet, so its predecessor's grant no longer applies.
	expect(handle.grant.peek()).toBeUndefined();
	expect(token.grant.peek()).toBeUndefined();

	answer(successor.server.auth.requests(), (token) => (token ? grant(["second"], []) : grant([], [])));
	await late;
	await waitFor(handle.grant, (g) => g?.publish.equals(patterns("second")) === true);
	for (let i = 0; i < 10; i++) await yieldNow();
	expect(seen).not.toContain("late");

	client.abort();
	server.abort();
	successor.client.abort();
	successor.server.abort();
});

test("lite-07 refuses a token added to a session's own handle after a GOAWAY", async () => {
	const protocol = Lite.ALPN_07_WIP;
	const pair = createMockTransportPair(protocol);
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client, publish: new OriginProducer().consume() }),
		acceptSession({ transport: pair.server, url }),
	]);
	const stream = await Stream.open(pair.server, { version: Lite.Version.DRAFT_07 });
	await stream.writer.u53(Lite.StreamId.Goaway);
	await new Lite.Goaway("").encode(stream.writer, Lite.Version.DRAFT_07);
	await settles(() => !wireOf(client).auth.active);

	try {
		expect(() => client.auth.add("late")).toThrow(StreamError);
	} finally {
		client.abort();
		server.abort();
	}
});

test("lite-07 ignores a draining session's late refusal once its successor granted", async () => {
	const protocol = Lite.ALPN_07_WIP;
	const handle = new Handle();
	const pair = createMockTransportPair(protocol);
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client, publish: new OriginProducer().consume(), auth: handle }),
		acceptSession({ transport: pair.server, url }),
	]);
	const requests = server.auth.requests();
	const setup = await requests.next();
	if (!setup) throw new Error("missing setup token");
	const issued = [setup.accept(grant([], []))];
	const adding = answered(handle.add("token"));
	const unanswered = await requests.next();
	expect(unanswered && new TextDecoder().decode(unanswered.token)).toBe("token");

	const stream = await Stream.open(pair.server, { version: Lite.Version.DRAFT_07 });
	await stream.writer.u53(Lite.StreamId.Goaway);
	await new Lite.Goaway("").encode(stream.writer, Lite.Version.DRAFT_07);
	await settles(() => !wireOf(client).auth.active);

	const successor = await connect({ publish: new OriginProducer(), protocol, auth: handle });
	answer(successor.server.auth.requests(), (token) => (token ? grant(["second"], []) : grant([], [])));
	const token = await adding;
	await waitFor(token.grant, (g) => g?.publish.equals(patterns("second")) === true);

	const warned = spyOn(console, "warn").mockImplementation(() => {});
	let ended = false;
	void token.closed.then(() => {
		ended = true;
	});
	unanswered?.reject(SessionCode.Unauthorized, "stale");
	try {
		await settles(() => warned.mock.calls.some(([line]) => line === "auth token refused"));
		for (let i = 0; i < 10; i++) await yieldNow();
		expect(ended).toBe(false);
		expect(token.grant.peek()?.publish.equals(patterns("second"))).toBe(true);
	} finally {
		warned.mockRestore();
		for (const grant of issued) grant.close();
		client.abort();
		server.abort();
		successor.client.abort();
		successor.server.abort();
	}
});

test("lite-07 ignores a draining session's late refusal while its successor retries", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const warned = spyOn(console, "warn").mockImplementation(() => {});
	const refusals = () => warned.mock.calls.filter(([line]) => line === "auth token refused").length;
	const protocol = Lite.ALPN_07_WIP;
	const handle = new Handle();
	const pair = createMockTransportPair(protocol);
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client, publish: new OriginProducer().consume(), auth: handle }),
		acceptSession({ transport: pair.server, url }),
	]);
	const requests = server.auth.requests();
	const issued: Issued[] = [];
	const setup = await requests.next();
	if (!setup) throw new Error("missing setup token");
	issued.push(setup.accept(grant([], [])));
	const adding = answered(handle.add("token"));
	const unanswered = await requests.next();
	expect(unanswered && new TextDecoder().decode(unanswered.token)).toBe("token");

	const stream = await Stream.open(pair.server, { version: Lite.Version.DRAFT_07 });
	await stream.writer.u53(Lite.StreamId.Goaway);
	await new Lite.Goaway("").encode(stream.writer, Lite.Version.DRAFT_07);
	await settles(() => !wireOf(client).auth.active);

	const next = await connect({ publish: new OriginProducer(), protocol, auth: handle });
	const successorRequests = next.server.auth.requests();
	try {
		const successorSetup = await successorRequests.next();
		if (!successorSetup) throw new Error("missing successor setup token");
		issued.push(successorSetup.accept(grant([], [])));
		const first = await successorRequests.next();
		if (!first) throw new Error("the successor did not present the token");
		const firstGrant = first.accept(grant(["second"], []));
		const token = await adding;
		await waitFor(token.grant, (g) => g?.publish.equals(patterns("second")) === true);
		let ended = false;
		void token.closed.then(() => {
			ended = true;
		});

		// The successor's grant ends with a refusal that may pass, so it backs off with no grant.
		const before = refusals();
		firstGrant.revoke(SessionCode.TooManyRequests, "busy");
		await settles(() => refusals() > before);
		await waitFor(token.grant, (g) => g === undefined);

		// The draining session's stale refusal lands during that backoff.
		const stale = refusals();
		unanswered?.reject(SessionCode.Unauthorized, "stale");
		await settles(() => refusals() > stale);
		for (let i = 0; i < 10; i++) await yieldNow();
		expect(ended).toBe(false);

		const retried = successorRequests.next();
		await clock.advance(Duration.millis(1000));
		const again = await retried;
		if (!again) throw new Error("the successor did not present the token again");
		issued.push(again.accept(grant(["third"], [])));
		await waitFor(token.grant, (g) => g?.publish.equals(patterns("third")) === true);
		expect(ended).toBe(false);
	} finally {
		for (const grant of issued) grant.close();
		client.abort();
		server.abort();
		next.client.abort();
		next.server.abort();
		warned.mockRestore();
		installed.close();
	}
});

// moq-lite carries patterns, so every grant arrives exactly as issued.
test("lite-07 a token granted on a draining session stays held until its successor answers", async () => {
	const protocol = Lite.ALPN_07_WIP;
	const handle = new Handle();
	const pair = createMockTransportPair(protocol);
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client, publish: new OriginProducer().consume(), auth: handle }),
		acceptSession({ transport: pair.server, url }),
	]);
	const requests = server.auth.requests();
	const setup = await requests.next();
	if (!setup) throw new Error("missing setup token");
	const issued = [setup.accept(grant([], []))];
	const token = handle.present("token");
	const request = await requests.next();
	if (!request) throw new Error("missing token");
	issued.push(request.accept(grant(["a"], [])));
	await token.answered;
	expect(token.held).toBe(true);

	const stream = await Stream.open(pair.server, { version: Lite.Version.DRAFT_07 });
	await stream.writer.u53(Lite.StreamId.Goaway);
	await new Lite.Goaway("").encode(stream.writer, Lite.Version.DRAFT_07);
	await settles(() => !wireOf(client).auth.active);

	const next = await connect({ publish: new OriginProducer(), protocol, auth: handle });
	const successorRequests = next.server.auth.requests();
	try {
		const successorSetup = await successorRequests.next();
		if (!successorSetup) throw new Error("missing successor setup token");
		issued.push(successorSetup.accept(grant([], [])));
		expect(await successorRequests.next()).toBeDefined();
		// The live session has not answered, but the draining one still grants it.
		expect(token.grant.peek()).toBeUndefined();
		expect(token.held).toBe(true);
	} finally {
		client.abort();
		server.abort();
		next.client.abort();
		next.server.abort();
	}
});

test("a fetch abandoned while its token waits never reaches the wire", async () => {
	const logged = spyOn(console, "debug");
	const origin = new OriginProducer();
	const broadcastRoute = origin.route(Path.from("room/x"));
	const broadcast = broadcastRoute.broadcast;
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
	const adding = answered(client.auth.add("first"));
	try {
		const unanswered = await requests.next();
		if (!unanswered) throw new Error("missing token");
		const abort = new AbortController();
		const fetched = wireOf(client)
			.consume(Path.from("room/x"))
			.track("video")
			.fetchGroup(0n, { signal: abort.signal });
		await yieldNow();
		abort.abort(new Error("left"));
		await expect(fetched).rejects.toThrow();
		await settles(() => waited(logged, "room/x"));
		const opened = transport.sendStreams.bidi.length;
		const issued = unanswered.accept(grant([], ["room"]));
		await adding;
		for (let i = 0; i < 10; i++) await yieldNow();
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

// The peer refuses a listing its token has not yet authorized, so the request waits for it.
test("a listing waits for its token before reaching the wire", async () => {
	const origin = new OriginProducer();
	const broadcastRoute = origin.route(Path.from("room/x"));
	const broadcast = broadcastRoute.broadcast;
	broadcast.createTrack("video");
	const { client, server, transport } = await connect({ serverPublish: origin, protocol: Lite.ALPN_07_WIP });
	const requests = server.auth.requests();
	const setup = await requests.next();
	if (!setup) throw new Error("missing setup token");
	const initial = setup.accept(grant([], []));
	await waitFor(client.auth.grant, (g) => g !== undefined);
	const adding = answered(client.auth.add("first"));
	try {
		const unanswered = await requests.next();
		if (!unanswered) throw new Error("missing token");
		const opened = transport.sendStreams.bidi.length;
		const tracks = wireOf(client).consume(Path.from("room/x")).tracks();
		for (let i = 0; i < 10; i++) await yieldNow();
		expect(transport.sendStreams.bidi.length).toBe(opened);
		const issued = unanswered.accept(grant([], ["room"]));
		await adding;
		expect(await tracks.next()).toMatchObject({ kind: "added", name: "video" });
		expect(await tracks.next()).toEqual({ kind: "synced" });
		tracks.close();
		issued.close();
	} finally {
		initial.close();
		client.abort();
		server.abort();
		await adding.catch(() => void 0);
	}
});

// The window between the wait seeing the caller leave and acting on it spans a few
// microtasks, so sweep where the new caller lands.
test.each(Array.from({ length: 17 }, (_, gap) => gap))(
	"a fetch that regains a caller while its token waits keeps waiting (%d)",
	async (gap) => {
		const origin = new OriginProducer();
		const broadcastRoute = origin.route(Path.from("room/x"));
		const broadcast = broadcastRoute.broadcast;
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
		const adding = answered(client.auth.add("first"));
		try {
			const unanswered = await requests.next();
			if (!unanswered) throw new Error("missing token");
			const track = wireOf(client).consume(Path.from("room/x")).track("video");
			const abort = new AbortController();
			const first = track.fetchGroup(0n, { signal: abort.signal });
			await yieldNow();
			// The last caller leaves and another coalesces before the wait observes it.
			abort.abort(new Error("left"));
			for (let i = 0; i < gap; i++) await Promise.resolve();
			const second = track.fetchGroup(0n);
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

test.each([Lite.ALPN_07_WIP])("%s carries literal and wildcard grants exactly", async (protocol) => {
	const { client, server } = await connect({ publish: new OriginProducer(), protocol });
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
		const added = await answered(client.auth.add(token));
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
	[Ietf.ALPN.DRAFT_22, true],
])("%s has no grant, keeping a token for requests: %p", async (protocol, kept) => {
	const { client, server } = await connect({ publish: new OriginProducer(), protocol });
	expect(client.auth.grant.peek()).toBeUndefined();
	if (kept) {
		const token = await answered(client.auth.add("token"));
		expect(token.grant.peek()).toBeUndefined();
		expect(client.auth.grant.peek()).toBeUndefined();
		// A request carries the bytes as added, whatever the caller does with its buffer after.
		const bytes = new TextEncoder().encode("bytes");
		const copied = await answered(client.auth.add(bytes));
		bytes.fill(0);
		expect(wireOf(client).auth.carried(copied)).toEqual(new TextEncoder().encode("bytes"));
		// The token ends with its session, like any other.
		client.abort();
		expect(await token.closed).toBeInstanceOf(Error);
	} else {
		await expect(answered(client.auth.add("token"))).rejects.toBeInstanceOf(Unsupported);
	}
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
	const upRoute = clientOrigin.route(Path.from("up/y"));
	const up = upRoute.broadcast;
	const upTrack = up.createTrack("video");

	const serverOrigin = new OriginProducer();
	const downRoute = serverOrigin.route(Path.from("room/x"));
	const down = downRoute.broadcast;
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
		const live = answered(client.auth.add("live"));
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

test("grants issued while a reply is blocked replace the one still queued", async () => {
	const { client, server } = await connect({ publish: new OriginProducer(), protocol: Lite.ALPN_07_WIP });
	const grants = Array.from({ length: 100 }, (_, i) => grant([`${i}`], []));
	const original = LiteAuthWire.prototype.grant;
	// Which of `grants` reached the wire, in order.
	const sent: number[] = [];
	let release!: () => void;
	const blocked = new Promise<void>((resolve) => {
		release = resolve;
	});
	const spy = spyOn(LiteAuthWire.prototype, "grant").mockImplementation(async function (
		this: LiteAuthWire,
		stream,
		grant,
	) {
		const index = grants.findIndex((issued) => issued.publish === grant.publish);
		if (index >= 0) sent.push(index);
		if (index === 0) await blocked;
		return original.call(this, stream, grant);
	});
	try {
		const requests = server.auth.requests();
		const setup = await requests.next();
		if (!setup) throw new Error("setup token missing");
		setup.accept(grant(["a"], []));
		const live = answered(client.auth.add("live"));
		live.catch(() => void 0);
		const request = await requests.next();
		if (!request) throw new Error("live token missing");
		const issued = request.accept(grants[0]);
		await settles(() => sent.length === 1);
		for (const update of grants.slice(1)) issued.update(update);
		release();
		await live;
		await settles(() => sent.length === 2);
		for (let i = 0; i < 10; i++) await yieldNow();
		expect(sent).toEqual([0, 99]);
	} finally {
		spy.mockRestore();
		client.abort();
		server.abort();
	}
});

// Capture the acceptor's actual AUTH writer after a valid setup grant. The following
// bytes travel through the same connection dispatcher and presenter as a real peer.
describe.each([Lite.ALPN_07_WIP])("AUTH violations %s", (protocol) => {
	test("a second presenter message closes the session", async () => {
		const streams: Stream[] = [];
		const prototype = LiteAuthWire.prototype;
		const original = prototype.present;
		const spy = spyOn(prototype, "present").mockImplementation(async function (this: LiteAuthWire, token) {
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
				(await withTimeout(pair.transport.closed, Duration.millis(100), "second AUTH did not close")).closeCode,
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
		const spy = spyOn(LiteAuthMessage.prototype, "encode").mockImplementation(async (writer: Writer) => {
			await LiteMessage.encode(writer, async (w: Writer) => {
				await w.u53(2);
			});
		});
		const added = answered(pair.client.auth.add("bad")).catch(() => undefined);
		try {
			expect(
				(await withTimeout(pair.transport.closed, Duration.millis(100), "malformed AUTH did not close"))
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
			const prototype = LiteAuthWire.prototype;
			const original = prototype.grant;
			const spy = spyOn(prototype, "grant").mockImplementation(async function (
				this: LiteAuthWire,
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
					const id = fault === "oversized expiry" ? 0 : 1;
					await LiteMessage.encode(stream.writer, body, { id });
				}
				expect(
					(await withTimeout(pair.transport.closed, Duration.millis(100), "malformed AUTH did not close"))
						.closeCode,
				).toBe(SessionCode.ProtocolViolation);
			} finally {
				spy.mockRestore();
				pair?.client.abort();
				pair?.server.abort();
			}
		});
	}
});
