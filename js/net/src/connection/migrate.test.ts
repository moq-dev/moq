import { afterEach, expect, test } from "bun:test";
import { Milli } from "@moq/clock";
import { RefusedRedirect } from "../error.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { accept } from "./accept.ts";
import { Connection, resetShared } from "./pool.ts";
import { Reload } from "./reload.ts";

const original = globalThis.WebTransport;

afterEach(() => {
	resetShared();
	globalThis.WebTransport = original;
});

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

// Polls until `pred` holds, so a regression fails the test instead of hanging it.
async function waitUntil(pred: () => boolean, ms = 2000): Promise<void> {
	const deadline = Date.now() + ms;
	for (;;) {
		if (pred()) return;
		if (Date.now() > deadline) throw new Error("timed out waiting for condition");
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
const handover = Milli(200);

test("an empty GOAWAY migrates the announced request to the replacement session", async () => {
	const { dials, origin } = fleet();
	const broadcast = origin.createBroadcast(Path.from("cam"));
	broadcast.announce();

	const consume = new OriginProducer();
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		consume,
		goaway: { handover },
		// A session younger than this counts as redirected immediately and backs off first.
		delay: { initial: Milli(1) },
	});
	const watched = consume.request(Path.from("cam"), { announced: true });

	try {
		await waitUntil(() => watched.active.peek() !== undefined);
		const first = reload.established.peek();
		const served = watched.active.peek();

		const drained = dials[0];
		if (!drained) throw new Error("no first dial");
		await goaway(drained.server, "");

		// The replacement dials the configured URL at once, while the old session still serves.
		await waitUntil(() => dials.length === 2);
		expect(dials[1]?.url).toBe(url.href);
		await waitUntil(() => reload.established.peek() !== first && reload.established.peek() !== undefined);
		expect(drained.closed).toBe(false);
		expect(reload.status.peek()).toBe("connected");

		// Without an epoch the replacement route is a new publisher instance.
		// The announced request follows it while the old session finishes draining.
		await waitUntil(() => watched.active.peek() !== undefined && watched.active.peek() !== served);
		expect(watched.closed.peek()).toBeUndefined();
		await waitUntil(() => drained.closed, handover * 10);
		expect(watched.active.peek()).toBeDefined();
		expect(watched.closed.peek()).toBeUndefined();
		expect(dials.length).toBe(2);
	} finally {
		watched.close();
		reload.close();
		consume.close();
		broadcast.close();
	}
});

test("a GOAWAY without a timeout hands over at the configured cap", async () => {
	const { dials } = fleet();
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		goaway: { handover },
		delay: { initial: Milli(1) },
	});

	try {
		await waitUntil(() => reload.status.peek() === "connected");
		const drained = dials[0];
		if (!drained) throw new Error("no first dial");

		const sent = performance.now();
		await goaway(drained.server, "");
		await waitUntil(() => dials.length === 2 && reload.status.peek() === "connected");

		// Lite carries no deadline, which must read as "the cap", never as a zero handover.
		await waitUntil(() => drained.closed, handover * 10);
		expect(performance.now() - sent).toBeGreaterThanOrEqual(handover * 0.9);
	} finally {
		reload.close();
	}
});

test("a refused redirect ends the connection instead of reconnecting", async () => {
	const refused = ["https://other.example/", "not a url", "http://relay.example/", "https://127.0.0.1/"];
	for (const uri of refused) {
		const { dials } = fleet();
		const reload = new Reload({ url, websocket: { enabled: false }, delay: { initial: Milli(1) } });

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
			await new Promise((resolve) => setTimeout(resolve, 50));
			expect(dials.length, uri).toBe(1);
		} finally {
			reload.close();
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
		delay: { initial: Milli(1) },
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
	const { dials } = fleet();

	const handle = new Connection({ url });
	try {
		await waitUntil(() => handle.status.peek() === "connected");
		const origin = handle.origin.peek();

		await goaway(dials[0]?.server as WebTransport, moved.href);
		await waitUntil(() => dials.length === 2);
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
	}
});

test("a redirect onto an already pooled key leaves both entries to their handles", async () => {
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
		await waitUntil(() => dials.filter((dial) => dial.url === moved.href).length === 2);
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
	}
});

/**
 * A fleet where every session redirects to the other address once it has outlived `life`,
 * so each hop looks healthy to the initial-delay damping.
 */
function bouncing(life: number): { dials: Dial[]; stop: () => void } {
	const { dials } = fleet();
	const timers = new Set<ReturnType<typeof setTimeout>>();
	const stub = globalThis.WebTransport as unknown as (url: string | URL) => WebTransport;
	globalThis.WebTransport = function BouncingWebTransport(target: string | URL) {
		const client = stub(target);
		const dial = dials[dials.length - 1] as Dial;
		const other = new URL(target).href === url.href ? moved.href : url.href;
		const timer = setTimeout(() => {
			timers.delete(timer);
			if (!dial.closed) void goaway(dial.server, other).catch(() => {});
		}, life);
		timers.add(timer);
		return client;
	} as unknown as typeof WebTransport;
	return {
		dials,
		stop: () => {
			for (const timer of timers) clearTimeout(timer);
		},
	};
}

test("two peers redirecting to each other escalate the backoff instead of cycling", async () => {
	const { dials, stop } = bouncing(40);
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		goaway: { handover },
		// Every session outlives this, so the immediate-redirect damping never applies.
		delay: { initial: Time.Milli(10), multiplier: 4, max: Time.Milli(1000) },
	});

	try {
		await new Promise((resolve) => setTimeout(resolve, 1000));
		// Without escalation this alternates roughly every session lifetime (~16 dials).
		expect(dials.length).toBeLessThanOrEqual(6);
		// The ping-pong is a loop to slow down, not a failure: the redirects are honoured.
		expect(dials.slice(0, 3).map((dial) => dial.url)).toEqual([url.href, moved.href, url.href]);
		expect(reload.error.peek()).toBeUndefined();
	} finally {
		stop();
		reload.close();
	}
});

test("a redirect loop stops at the retry window", async () => {
	const { stop } = bouncing(40);
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		goaway: { handover },
		delay: { initial: Time.Milli(10), multiplier: 4, max: Time.Milli(1000), timeout: Time.Milli(150) },
	});

	try {
		await waitUntil(() => reload.error.peek() !== undefined, 3000);
		expect(reload.error.peek()?.message).toContain("redirect loop");
	} finally {
		stop();
		reload.close();
	}
});

test("a one-way rolling restart migrates without backing off", async () => {
	const { dials } = fleet();
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		goaway: { handover },
		// Long enough that any backoff would be visible.
		delay: { initial: Time.Milli(1000) },
	});

	try {
		await waitUntil(() => reload.status.peek() === "connected");
		await new Promise((resolve) => setTimeout(resolve, 1100));
		const started = performance.now();
		await goaway(dials[0]?.server as WebTransport, moved.href);
		await waitUntil(() => dials.length === 2, 500);
		expect(performance.now() - started).toBeLessThan(500);
	} finally {
		reload.close();
	}
});

test("repeated GOAWAYs back to the same place are restarts, not a loop", async () => {
	const { dials } = fleet();
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		goaway: { handover },
		// Long enough that any backoff would be visible.
		delay: { initial: Time.Milli(300) },
	});

	try {
		for (let i = 1; i <= 3; i++) {
			await waitUntil(() => dials.length === i && reload.status.peek() === "connected");
			// Outlive the initial delay so only loop detection could slow this down.
			await new Promise((resolve) => setTimeout(resolve, 350));
			const started = performance.now();
			await goaway(dials[i - 1]?.server as WebTransport, "");
			await waitUntil(() => dials.length === i + 1, 1000);
			expect(performance.now() - started).toBeLessThan(150);
		}
	} finally {
		reload.close();
	}
});
