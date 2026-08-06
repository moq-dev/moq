import { afterEach, expect, test } from "bun:test";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { accept } from "./index.ts";
import { resetShared, Shared } from "./pool.ts";

const url = new URL("https://example.com/pool");

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

// Polls until `pred` holds, so a regression fails the test instead of hanging it.
async function waitUntil(pred: () => boolean): Promise<void> {
	for (let i = 0; i < 500; i++) {
		if (pred()) return;
		await settle();
	}
	throw new Error("timed out waiting for condition");
}

// A tiny window keeps the linger tests quick without mocking timers. The wait is a wide
// multiple of it so a loaded runner's timer drift can't be mistaken for lingering.
const linger = 20;

async function expired() {
	await new Promise((resolve) => setTimeout(resolve, linger * 15));
}

const original = globalThis.WebTransport;

afterEach(() => {
	resetShared();
	globalThis.WebTransport = original;
});

/** Hand out a live mock transport per dial, counting how many were opened. */
function stubTransports(): { count: () => number } {
	let count = 0;
	const stub = function StubWebTransport() {
		count++;
		const pair = createMockTransportPair(Lite.ALPN_05);
		void accept(pair.server, url);
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;
	return { count: () => count };
}

test("two handles on one URL share a connection and an origin", async () => {
	const dials = stubTransports();

	const first = new Shared({ url, linger });
	const second = new Shared({ url });

	await waitUntil(() => first.status.peek() === "connected");
	await waitUntil(() => second.status.peek() === "connected");
	expect(dials.count()).toBe(1);
	expect(second.origin.peek()).toBe(first.origin.peek());

	// One handle leaving doesn't disturb the other.
	first.close();
	await expired();
	expect(second.status.peek()).toBe("connected");

	second.close();
});

test("the connection closes after the last handle and the linger window", async () => {
	const dials = stubTransports();

	const handle = new Shared({ url, linger });
	await waitUntil(() => handle.status.peek() === "connected");
	const origin = handle.origin.peek();

	handle.close();
	// Idempotent: a double close must not double-release.
	handle.close();

	await expired();
	expect(origin?.closed.peek()).not.toBeUndefined();

	// The next handle dials fresh.
	const next = new Shared({ url, linger });
	await waitUntil(() => next.status.peek() === "connected");
	expect(dials.count()).toBe(2);
	next.close();
});

test("a handle taken within the linger window reuses the warm connection", async () => {
	const dials = stubTransports();

	const first = new Shared({ url, linger: 10_000 });
	await waitUntil(() => first.status.peek() === "connected");
	const origin = first.origin.peek();
	first.close();

	const second = new Shared({ url });
	await waitUntil(() => second.origin.peek() !== undefined);
	expect(dials.count()).toBe(1);
	expect(second.origin.peek()).toBe(origin);
	second.close();
});

test("disabling a handle releases its share", async () => {
	stubTransports();

	const toggled = new Shared({ url, linger });
	const steady = new Shared({ url });
	await waitUntil(() => toggled.status.peek() === "connected");

	toggled.enabled.set(false);
	await waitUntil(() => toggled.origin.peek() === undefined);
	expect(toggled.status.peek()).not.toBe("connected");

	// The steady handle keeps the connection alive through the toggle.
	await expired();
	expect(steady.status.peek()).toBe("connected");

	// Re-enabling rejoins the shared connection.
	toggled.enabled.set(true);
	await waitUntil(() => toggled.status.peek() === "connected");
	expect(toggled.origin.peek()).toBe(steady.origin.peek());

	toggled.close();
	steady.close();
});

test("switching URLs switches origins", async () => {
	const dials = stubTransports();

	const handle = new Shared({ url, linger });
	await waitUntil(() => handle.origin.peek() !== undefined);
	const before = handle.origin.peek();

	handle.url.set(new URL("https://example.com/other"));
	await waitUntil(() => handle.origin.peek() !== undefined && handle.origin.peek() !== before);
	expect(dials.count()).toBe(2);

	handle.close();
});

test("a publish through one handle resolves locally for another", async () => {
	stubTransports();

	const publisher = new Shared({ url, linger });
	const watcher = new Shared({ url });
	await waitUntil(() => publisher.origin.peek() !== undefined);

	const origin = publisher.origin.peek();
	if (!origin) throw new Error("expected an origin");
	const broadcast = origin.publish(Path.from("mine"));
	broadcast.createTrack("chat");

	// Loopback: the shared origin serves the page's own publish with no round trip.
	const handle = watcher.origin.peek()?.get(Path.from("mine"));
	expect(handle).toBeDefined();
	handle?.close();

	broadcast.close();
	publisher.close();
	watcher.close();
});
