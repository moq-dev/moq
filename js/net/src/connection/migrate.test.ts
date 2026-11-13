import { afterEach, expect, spyOn, test } from "bun:test";
import { Duration, now, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { RefusedRedirect } from "../error.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { accept } from "./accept.ts";
import { Connection, resetShared } from "./pool.ts";
import { Redirects, Reload } from "./reload.ts";

const original = globalThis.WebTransport;

afterEach(() => {
	resetShared();
	globalThis.WebTransport = original;
});

async function settle() {
	await yieldNow();
}

// Polls until `pred` holds, so a regression fails the test instead of hanging it.
async function waitUntil(pred: () => boolean, ms = 2000): Promise<void> {
	const deadline = now().saturatingAdd(Duration.millis(ms));
	for (;;) {
		if (pred()) return;
		if (now().gt(deadline)) throw new Error("timed out waiting for condition");
		await settle();
	}
}

/** One accepted session on the fake fleet: the URL it was dialed at, and its server side. */
interface Dial {
	url: string;
	server: WebTransport;
	closed: boolean;
}

/**
 * Stand in for a fleet of relays behind any URL: every dial gets a fresh session serving
 * `origin`, so each one publishes the same broadcasts the way siblings of a fleet do.
 */
function fleet(origin = new OriginProducer()): { dials: Dial[]; origin: OriginProducer } {
	const dials: Dial[] = [];
	const stub = function StubWebTransport(url: string | URL) {
		const pair = createMockTransportPair(Lite.ALPN_06);
		const dial: Dial = { url: new URL(url).href, server: pair.server, closed: false };
		dials.push(dial);
		void pair.server.closed.then(
			() => {
				dial.closed = true;
			},
			() => {
				dial.closed = true;
			},
		);
		void accept({ transport: pair.server, url: new URL(url), publish: origin.consume() });
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;
	return { dials, origin };
}

/** Send a moq-lite GOAWAY from the server side of a session. */
async function goaway(server: WebTransport, uri: string): Promise<void> {
	const stream = await Stream.open(server, { version: Lite.Version.DRAFT_06 });
	await stream.writer.u53(Lite.StreamId.Goaway);
	await new Lite.Goaway(uri).encode(stream.writer, Lite.Version.DRAFT_06);
}

const url = new URL("https://relay.example/room");

// Short enough to watch, long enough that a handover visibly overlaps the replacement.
const handover = Duration.millis(200);

test("an empty GOAWAY migrates the follower to the replacement session", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const { dials, origin } = fleet();
	const broadcastRoute = origin.route(Path.from("cam"));
	const broadcast = broadcastRoute.broadcast;

	const consume = new OriginProducer();
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		consume,
		goaway: { handover },
		// A session younger than this counts as redirected immediately and backs off first.
		delay: { initial: Duration.millis(1) },
	});
	const watched = consume.follow(Path.from("cam"));

	try {
		await waitUntil(() => watched.active.peek() !== undefined);
		const first = reload.established.peek();
		const served = watched.active.peek();

		const drained = dials[0];
		if (!drained) throw new Error("no first dial");
		await goaway(drained.server, "");

		// The replacement dials the configured URL after the young session's backoff, while the old
		// session still serves.
		await elapse(clock, () => dials.length === 2);
		expect(dials[1]?.url).toBe(url.href);
		await elapse(clock, () => reload.established.peek() !== first && reload.established.peek() !== undefined);
		expect(drained.closed).toBe(false);
		expect(reload.status.peek()).toBe("connected");

		// Without an instance the replacement route is a new publisher instance.
		// The follower follows it while the old session finishes draining.
		await elapse(clock, () => watched.active.peek() !== undefined && watched.active.peek() !== served);
		expect(watched.closed.peek()).toBeUndefined();
		await elapse(clock, () => drained.closed);
		expect(watched.active.peek()).toBeDefined();
		expect(watched.closed.peek()).toBeUndefined();
		expect(dials.length).toBe(2);
	} finally {
		watched.close();
		reload.close();
		consume.close();
		broadcast.close();
		installed.close();
	}
});

test("a GOAWAY without a timeout hands over at the configured cap", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const { dials } = fleet();
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		goaway: { handover },
		delay: { initial: Duration.millis(1) },
	});

	try {
		await waitUntil(() => reload.status.peek() === "connected");
		const drained = dials[0];
		if (!drained) throw new Error("no first dial");

		const sent = clock.now();
		await goaway(drained.server, "");
		await elapse(clock, () => dials.length === 2 && reload.status.peek() === "connected");

		// Lite carries no deadline, which must read as "the cap", never as a zero handover.
		await elapse(clock, () => drained.closed);
		expect(clock.now().since(sent).ge(handover)).toBe(true);
	} finally {
		reload.close();
		installed.close();
	}
});

test("a refused redirect ends the connection instead of reconnecting", async () => {
	const refused = ["https://other.example/", "not a url", "http://relay.example/", "https://127.0.0.1/"];
	for (const uri of refused) {
		const clock = new Manual();
		const installed = clock.install();
		const { dials } = fleet();
		const reload = new Reload({ url, websocket: { enabled: false }, delay: { initial: Duration.millis(1) } });

		try {
			await waitUntil(() => reload.status.peek() === "connected");
			const drained = dials[0];
			if (!drained) throw new Error("no first dial");

			await goaway(drained.server, uri);
			await waitUntil(() => reload.error.peek() !== undefined);
			expect(reload.error.peek(), uri).toBeInstanceOf(RefusedRedirect);
			expect(reload.status.peek()).toBe("disconnected");
			await waitUntil(() => drained.closed);

			// Nothing redials: not the original URL, not anything else.
			await clock.advance(Duration.millis(50));
			expect(dials.length, uri).toBe(1);
		} finally {
			reload.close();
			installed.close();
		}
	}
});

test("a certificate pin refuses a redirect to another host even under follow", async () => {
	const { dials } = fleet();
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		webtransport: { serverCertificateHashes: [{ value: "00".repeat(32) }] },
		goaway: { redirect: "follow" },
		delay: { initial: Duration.millis(1) },
	});

	try {
		await waitUntil(() => reload.status.peek() === "connected");
		await goaway(dials[0]?.server as WebTransport, "https://other.example/");
		await waitUntil(() => reload.error.peek() !== undefined);
		expect(reload.error.peek()).toBeInstanceOf(RefusedRedirect);
		expect(dials.length).toBe(1);
	} finally {
		reload.close();
	}
});

// A same-host move to another port: what `same-host` exists to allow.
const moved = new URL("https://relay.example:5443/room");

test("a redirect moves the pool key while the handle keeps its origin", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const { dials } = fleet();

	const handle = new Connection({ url });
	try {
		await waitUntil(() => handle.status.peek() === "connected");
		const origin = handle.origin.peek();

		await goaway(dials[0]?.server as WebTransport, moved.href);
		// A session this young counts as redirected immediately, so the redial waits out a backoff.
		await elapse(clock, () => dials.length === 2);
		expect(dials[1]?.url).toBe(moved.href);
		expect(handle.origin.peek()).toBe(origin);

		// A caller configured with the target shares the migrated connection.
		const joined = new Connection({ url: moved });
		await waitUntil(() => joined.status.peek() === "connected");
		expect(joined.origin.peek()).toBe(origin);
		expect(dials.length).toBe(2);

		// One still asking for the original URL gets a fresh entry.
		const fresh = new Connection({ url });
		await waitUntil(() => fresh.status.peek() === "connected");
		expect(fresh.origin.peek()).not.toBe(origin);
		expect(dials.length).toBe(3);

		joined.close();
		fresh.close();
	} finally {
		handle.close();
		installed.close();
	}
});

// Replays RedialsALiveRelay in js/watch/verify/Failover.tla under FailoverRedirect.cfg: once the relay
// a redirect named dies, one failed dial to it drops the redirect and the redial returns to the
// configured URL, which still answers.
test("a redirected connection falls back to the configured URL once its target dies", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const { dials } = fleet();
	const accepting = globalThis.WebTransport;
	const attempts: string[] = [];
	let dead = false;
	globalThis.WebTransport = function DyingSibling(dialed: string | URL, options?: WebTransportOptions) {
		const href = new URL(dialed).href;
		attempts.push(href);
		if (dead && href === moved.href) throw new Error("connection refused");
		return new accepting(dialed, options);
	} as unknown as typeof WebTransport;
	// A pooled loop keeps the WebSocket fallback; keep it local, so only the stub decides.
	const socket = globalThis.WebSocket;
	globalThis.WebSocket = function NoFallback() {
		throw new Error("no fallback");
	} as unknown as typeof WebSocket;
	// Pooled, so the entry's key follows the URL the loop dials.
	const handle = new Connection({ url });

	try {
		await waitUntil(() => handle.status.peek() === "connected");
		await goaway(dials[0]?.server as WebTransport, moved.href);
		await elapse(clock, () => dials.length === 2 && handle.status.peek() === "connected");
		expect(dials[1]?.url).toBe(moved.href);

		// The relay it was sent to dies and stays down.
		dead = true;
		const before = attempts.length;
		dials[1]?.server.close();
		await elapse(clock, () => dials.length === 3 && handle.status.peek() === "connected");
		expect(attempts.slice(before)).toEqual([moved.href, url.href]);
		expect(dials[2]?.url).toBe(url.href);
		const origin = handle.origin.peek();

		// The entry is keyed on the configured URL again: a caller asking for it shares the
		// connection, and one asking for the dead target dials fresh.
		const joined = new Connection({ url });
		await waitUntil(() => joined.origin.peek() !== undefined);
		expect(joined.origin.peek()).toBe(origin);
		const fresh = new Connection({ url: moved });
		await waitUntil(() => fresh.origin.peek() !== undefined);
		expect(fresh.origin.peek()).not.toBe(origin);

		joined.close();
		fresh.close();
	} finally {
		globalThis.WebTransport = accepting;
		globalThis.WebSocket = socket;
		handle.close();
		installed.close();
	}
});

test("a redirect onto an already pooled key leaves both entries to their handles", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const { dials } = fleet();

	const redirected = new Connection({ url });
	const resident = new Connection({ url: moved });
	try {
		await waitUntil(() => redirected.status.peek() === "connected" && resident.status.peek() === "connected");
		const migrated = redirected.origin.peek();
		const target = resident.origin.peek();
		expect(migrated).not.toBe(target);

		const drained = dials.find((dial) => dial.url === url.href);
		await goaway(drained?.server as WebTransport, moved.href);
		await elapse(clock, () => dials.filter((dial) => dial.url === moved.href).length === 2);
		await waitUntil(() => redirected.status.peek() === "connected");

		// Existing handles keep their own entries.
		expect(redirected.origin.peek()).toBe(migrated);
		expect(resident.origin.peek()).toBe(target);

		// The target key still belongs to the entry that was there.
		const joined = new Connection({ url: moved });
		await waitUntil(() => joined.origin.peek() !== undefined);
		expect(joined.origin.peek()).toBe(target);

		// The original key was vacated, so a new caller dials fresh.
		const before = dials.length;
		const fresh = new Connection({ url });
		await waitUntil(() => fresh.status.peek() === "connected");
		expect(fresh.origin.peek()).not.toBe(migrated);
		expect(fresh.origin.peek()).not.toBe(target);
		expect(dials.length).toBe(before + 1);

		joined.close();
		fresh.close();
	} finally {
		redirected.close();
		resident.close();
		installed.close();
	}
});

test("the connection's tokens ride every session across a GOAWAY", async () => {
	const clock = new Manual();
	const installed = clock.install();
	// Bounded by task turns rather than wall time, advancing the clock a millisecond per turn
	// so the reconnect's delay passes long before the draining session's handover.
	const until = async (done: () => boolean) => {
		for (let i = 0; i < 100 && !done(); i++) {
			await yieldNow();
			await clock.advance(Duration.millis(1));
		}
		expect(done()).toBe(true);
	};

	// Every dial gets a lite-07 session that grants each token it is presented.
	const relays: { server: WebTransport; tokens: string[]; withdrawn: string[] }[] = [];
	const stub = function StubWebTransport(dialed: string | URL) {
		const pair = createMockTransportPair(Lite.ALPN_07_WIP);
		const relay = { server: pair.server, tokens: [] as string[], withdrawn: [] as string[] };
		relays.push(relay);
		void accept({ transport: pair.server, url: new URL(dialed) }).then((session) => {
			const requests = session.auth.requests();
			void (async () => {
				for (;;) {
					const request = await requests.next();
					if (!request) break;
					const token = new TextDecoder().decode(request.token);
					relay.tokens.push(token);
					const issued = request.accept({
						publish: new Path.Patterns([Path.Pattern.all()]),
						subscribe: new Path.Patterns(),
					});
					void issued.closed.then(() => relay.withdrawn.push(token));
				}
			})();
		});
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const reload = new Reload({
		url,
		websocket: { enabled: false },
		versions: [Lite.ALPN_07_WIP],
		goaway: { handover },
		delay: { initial: Duration.millis(1) },
	});
	try {
		const token = reload.auth.add("token");
		await token.answered;
		const first = relays[0];
		if (!first) throw new Error("no first dial");
		expect(first.tokens).toContain("token");

		const stream = await Stream.open(first.server, { version: Lite.Version.DRAFT_07 });
		await stream.writer.u53(Lite.StreamId.Goaway);
		await new Lite.Goaway("").encode(stream.writer, Lite.Version.DRAFT_07);

		// The replacement presents it with no help from the application, while the draining
		// session keeps presenting it beside its successor.
		await until(() => relays[1]?.tokens.includes("token") === true);
		await until(() => token.grant.peek() !== undefined && reload.established.peek() !== undefined);
		await yieldNow();
		expect(first.withdrawn).not.toContain("token");
		let ended = false;
		void token.closed.then(() => {
			ended = true;
		});
		await yieldNow();
		expect(ended).toBe(false);

		// The tokens end with the loop.
		reload.close();
		expect(await token.closed).toBeInstanceOf(Error);
	} finally {
		reload.close();
		installed.close();
	}
});

// How long each session on the bouncing fleet lives before its peer redirects it away.
const life = Duration.millis(40);

/** Fire `clock` timer by timer until `pred` holds, and return the time that took. */
async function elapse(clock: Manual, pred: () => boolean): Promise<number> {
	const start = clock.now();
	for (;;) {
		// Let the mock sessions run before deciding that time has to pass.
		for (let i = 0; i < 10 && !pred(); i++) await settle();
		if (pred()) return clock.now().since(start).asMillis();
		await clock.fire();
	}
}

/**
 * Let session `hop` live for `life`, then have its peer redirect it to the other address,
 * minting a fresh credential in the query or userinfo when `rotate` is set.
 * Returns once the client has acted on the GOAWAY, so any backoff timer is already armed.
 */
async function bounce(
	clock: Manual,
	reload: Reload,
	dials: Dial[],
	hop: number,
	rotate?: "query" | "userinfo",
): Promise<string> {
	await waitUntil(() => dials.length === hop + 1 && reload.status.peek() === "connected");
	const dial = dials[hop];
	if (!dial) throw new Error(`no dial ${hop}`);
	const peer = new URL(dial.url).port === moved.port ? url : moved;
	const next = new URL(peer);
	if (rotate === "query") next.searchParams.set("jwt", String(hop));
	if (rotate === "userinfo") {
		next.username = `token-${hop}`;
		next.password = `secret-${hop}`;
	}
	const other = next.href;
	await clock.advance(life);
	await goaway(dial.server, other);
	await waitUntil(() => reload.redirect.peek()?.href === other);
	return other;
}

test.each([
	["", undefined],
	[" even with fresh query credentials on every redirect", "query"],
	[" even with fresh userinfo on every redirect", "userinfo"],
] as const)("two peers redirecting to each other escalate the backoff instead of cycling%s", async (_, rotate) => {
	const clock = new Manual();
	const installed = clock.install();
	const { dials } = fleet();
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		goaway: { handover },
		// Every session outlives the healthy bar, so the immediate-redirect damping never applies.
		delay: {
			initial: Duration.millis(10),
			multiplier: 4,
			max: Duration.millis(1000),
			healthy: Duration.millis(10),
		},
	});

	try {
		// The first move is a healthy migration. Every return after it is a loop that waits out
		// the jittered backoff, between half and all of a delay that escalates to the cap.
		const delays = [0, 10, 40, 160, 640, 1000, 1000];
		for (const [hop, delay] of delays.entries()) {
			const other = await bounce(clock, reload, dials, hop, rotate);
			const waited = await elapse(clock, () => dials.length === hop + 2);
			expect(waited, `hop ${hop}`).toBeGreaterThanOrEqual(delay / 2);
			expect(waited, `hop ${hop}`).toBeLessThanOrEqual(delay);
			// The ping-pong is a loop to slow down, not a failure: the redirects are honoured.
			expect(dials[hop + 1]?.url).toBe(other);
		}
		expect(reload.error.peek()).toBeUndefined();
	} finally {
		reload.close();
		installed.close();
	}
});

test("redirect endpoint keys omit userinfo without changing the dial URL", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const { dials } = fleet();
	const from = new URL("https://first:secret@relay.example/room?jwt=a#x");
	const to = new URL("https://second:other@relay.example:5443/room?jwt=b#y");
	const moves = spyOn(Redirects.prototype, "move");
	const reload = new Reload({
		url: from,
		websocket: { enabled: false },
		goaway: { handover },
		delay: { initial: Duration.millis(10) },
	});

	try {
		await waitUntil(() => reload.status.peek() === "connected");
		await clock.advance(life);
		await goaway(dials[0]?.server as WebTransport, to.href);
		await elapse(clock, () => dials.length === 2);
		expect(moves.mock.calls).toEqual([["https://relay.example/room", "https://relay.example:5443/room"]]);
		expect(dials.map((dial) => dial.url)).toEqual([from.href, to.href]);
		expect(from.username).toBe("first");
		expect(from.password).toBe("secret");
	} finally {
		reload.close();
		moves.mockRestore();
		installed.close();
	}
});

test("a redirect loop stops at the retry window", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const { dials } = fleet();
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		goaway: { handover },
		delay: {
			initial: Duration.millis(10),
			multiplier: 4,
			max: Duration.millis(1000),
			healthy: Duration.millis(10),
			timeout: Duration.millis(150),
		},
	});

	try {
		for (let hop = 0; reload.error.peek() === undefined; hop++) {
			// Without the loop guard every hop is healthy and resets the window, so this never ends.
			if (hop > 10) throw new Error("the loop never gave up");
			await bounce(clock, reload, dials, hop);
			await elapse(clock, () => dials.length === hop + 2 || reload.error.peek() !== undefined);
		}
		expect(reload.error.peek()?.message).toContain("redirect loop");
	} finally {
		reload.close();
		installed.close();
	}
});

test("a loop no faster than the capped delay migrates without backing off", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const { dials } = fleet();
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		goaway: { handover },
		// Each session lives exactly the cap, so it already redials no faster than the backoff would.
		delay: { initial: Duration.millis(10), max: life, healthy: Duration.millis(10) },
	});

	try {
		for (let hop = 0; hop < 6; hop++) {
			await bounce(clock, reload, dials, hop);
			expect(await elapse(clock, () => dials.length === hop + 2), `hop ${hop}`).toBe(0);
		}
	} finally {
		reload.close();
		installed.close();
	}
});

test("a one-way rolling restart migrates without backing off", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const { dials } = fleet();
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		goaway: { handover },
		// Long enough that any backoff would be visible.
		delay: { initial: Duration.millis(1000) },
	});

	try {
		await waitUntil(() => reload.status.peek() === "connected");
		// Outlive the default healthy bar (1000ms) so only loop detection could slow this down.
		await clock.advance(Duration.millis(1100));
		await goaway(dials[0]?.server as WebTransport, moved.href);
		expect(await elapse(clock, () => dials.length === 2)).toBe(0);
	} finally {
		reload.close();
		installed.close();
	}
});

test("repeated GOAWAYs back to the same place are restarts, not a loop", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const { dials } = fleet();
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		goaway: { handover },
		// Long enough that any backoff would be visible.
		delay: { initial: Duration.millis(300), healthy: Duration.millis(300) },
	});

	try {
		for (let i = 1; i <= 3; i++) {
			await waitUntil(() => dials.length === i && reload.status.peek() === "connected");
			// Outlive the healthy bar so only loop detection could slow this down.
			await clock.advance(Duration.millis(350));
			await goaway(dials[i - 1]?.server as WebTransport, "");
			expect(await elapse(clock, () => dials.length === i + 1), `restart ${i}`).toBe(0);
		}
	} finally {
		reload.close();
		installed.close();
	}
});

test("a peer restarting through immediate GOAWAYs escalates the backoff but never spends the window", async () => {
	const clock = new Manual();
	const installed = clock.install();
	// Full delay every time, so the waits are exact.
	const random = spyOn(Math, "random").mockReturnValue(1);
	const { dials } = fleet();
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		goaway: { handover },
		// The waits add up to far more than the window, which a restart restarts after each one.
		delay: {
			initial: Duration.millis(10),
			multiplier: 2,
			max: Duration.millis(1000),
			timeout: Duration.millis(25),
		},
	});

	try {
		const waited: number[] = [];
		for (let i = 1; i <= 4; i++) {
			await waitUntil(() => dials.length === i && reload.status.peek() === "connected");
			await goaway(dials[i - 1]?.server as WebTransport, "");
			waited.push(await elapse(clock, () => dials.length === i + 1 || reload.error.peek() !== undefined));
		}
		expect(waited).toEqual([10, 20, 40, 80]);
		expect(reload.error.peek()).toBeUndefined();
	} finally {
		reload.close();
		random.mockRestore();
		installed.close();
	}
});
