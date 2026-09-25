import { afterEach, expect, test } from "bun:test";
import { RefusedRedirect } from "../error.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import * as Time from "../time.ts";
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
	const stream = await Stream.open(server);
	await stream.writer.u53(Lite.StreamId.Goaway);
	await new Lite.Goaway(uri).encode(stream.writer, Lite.Version.DRAFT_06);
}

const url = new URL("https://relay.example/room");

// Short enough to watch, long enough that a handover visibly overlaps the replacement.
const handover = Time.Milli(200);

test("an empty GOAWAY migrates without unrouting the path", async () => {
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
		delay: { initial: Time.Milli(1) },
	});
	const watched = consume.request(Path.from("cam"), { announced: true });

	try {
		await waitUntil(() => watched.active.peek() !== undefined);
		const first = reload.established.peek();

		// Record every moment the path had no route, from here on.
		let gaps = 0;
		const stop = watched.active.subscribe((active) => {
			if (active === undefined) gaps++;
		});

		const drained = dials[0];
		if (!drained) throw new Error("no first dial");
		await goaway(drained.server, "");

		// The replacement dials the configured URL at once, while the old session still serves.
		await waitUntil(() => dials.length === 2);
		expect(dials[1]?.url).toBe(url.href);
		await waitUntil(() => reload.established.peek() !== first && reload.established.peek() !== undefined);
		expect(drained.closed).toBe(false);
		expect(reload.status.peek()).toBe("connected");

		// The old session closes at the handover cap, and the path never went unrouted.
		await waitUntil(() => drained.closed, handover * 10);
		await settle();
		expect(watched.active.peek()).not.toBeUndefined();
		expect(gaps).toBe(0);
		expect(dials.length).toBe(2);
		stop();
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
		delay: { initial: Time.Milli(1) },
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
		const reload = new Reload({ url, websocket: { enabled: false }, delay: { initial: Time.Milli(1) } });

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
		delay: { initial: Time.Milli(1) },
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
