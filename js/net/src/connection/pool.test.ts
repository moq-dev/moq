import { afterEach, beforeEach, expect, test } from "bun:test";
import { ALPN_05 } from "../lite/version.ts";
import {
	createMockTransportPair,
	createPendingTransports,
	type MockTransport,
	type PendingTransports,
} from "../mock.ts";
import * as Path from "../path.ts";
import { connect } from "./connect.ts";
import { resetPool } from "./pool.ts";

const url = new URL("https://example.com/pool");

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

// A tiny window keeps the linger tests quick without mocking timers. The wait is a wide
// multiple of it so a loaded runner's timer drift can't be mistaken for the session lingering.
const grace = 20;

async function expired() {
	await new Promise((resolve) => setTimeout(resolve, grace * 15));
}

const original = globalThis.WebTransport;

beforeEach(() => {
	resetPool();
});

afterEach(() => {
	resetPool();
	globalThis.WebTransport = original;
});

/** Hand out a live mock transport per dial, counting how many were opened. */
function stubTransports(): { transports: MockTransport[]; servers: MockTransport[] } {
	const transports: MockTransport[] = [];
	const servers: MockTransport[] = [];

	// Biome forbids returning a value from a class constructor.
	function StubWebTransport(this: unknown) {
		const pair = createMockTransportPair(ALPN_05);
		transports.push(pair.client);
		servers.push(pair.server);
		return pair.client;
	}
	globalThis.WebTransport = StubWebTransport as unknown as typeof WebTransport;

	return { transports, servers };
}

/** Hand out a transport that never finishes connecting, counting dials and closes. */
function stubPending(): PendingTransports {
	const pending = createPendingTransports();
	globalThis.WebTransport = pending.transport;
	return pending;
}

test("two connections to one URL share a session", async () => {
	const { transports } = stubTransports();

	const first = await connect(url, { websocket: { enabled: false } });
	const second = await connect(url, { websocket: { enabled: false } });

	expect(transports.length).toBe(1);
	expect(second).not.toBe(first);
	expect(second.url.href).toBe(url.href);

	first.close();
	second.close();
});

test("the session survives until the last handle lets go", async () => {
	const { transports } = stubTransports();

	const first = await connect(url, { websocket: { enabled: false }, pool: { grace } });
	const second = await connect(url, { websocket: { enabled: false } });

	let closed = false;
	void transports[0]?.closed.then(() => {
		closed = true;
	});

	first.close();
	await expired();
	expect(closed).toBe(false);

	second.close();
	await expired();
	expect(closed).toBe(true);
});

test("a session reacquired within the grace period is the same one", async () => {
	const { transports } = stubTransports();

	const first = await connect(url, { websocket: { enabled: false }, pool: { grace } });
	first.close();

	const second = await connect(url, { websocket: { enabled: false } });
	expect(transports.length).toBe(1);

	let closed = false;
	void transports[0]?.closed.then(() => {
		closed = true;
	});

	await expired();
	expect(closed).toBe(false);

	second.close();
});

test("a session dropped after the grace period is dialed again", async () => {
	const { transports } = stubTransports();

	const first = await connect(url, { websocket: { enabled: false }, pool: { grace } });
	first.close();
	await expired();

	const second = await connect(url, { websocket: { enabled: false } });
	expect(transports.length).toBe(2);

	second.close();
});

test("a session that dies is evicted, even when closed rejects", async () => {
	class RejectingWebTransport {
		ready = Promise.resolve(undefined);
		// A real WebTransport rejects `closed` on an abnormal termination.
		closed = Promise.reject(new Error("connection reset"));
		protocol = ALPN_05;

		close() {}

		createBidirectionalStream() {
			return new Promise(() => {});
		}

		createUnidirectionalStream() {
			return new Promise(() => {});
		}

		incomingBidirectionalStreams = new ReadableStream();
		incomingUnidirectionalStreams = new ReadableStream();
		datagrams = { maxDatagramSize: 0, readable: new ReadableStream(), writable: new WritableStream() };
	}

	globalThis.WebTransport = RejectingWebTransport as unknown as typeof WebTransport;

	const first = await connect(url, { websocket: { enabled: false }, pool: { grace } });
	await settle();

	// The dead session is gone from the pool, so the next caller dials rather than
	// leasing a corpse.
	const second = await connect(url, { websocket: { enabled: false }, pool: { grace } });
	expect(second).not.toBe(first);

	first.close();
	second.close();
});

test("concurrent callers share one connection attempt", async () => {
	const pending = stubPending();

	const first = connect(url, { websocket: { enabled: false } }).catch(() => undefined);
	const second = connect(url, { websocket: { enabled: false } }).catch(() => undefined);

	await settle();
	expect(pending.connects()).toBe(1);

	resetPool();
	await Promise.all([first, second]);
});

test("one caller aborting mid-connect leaves the attempt alone", async () => {
	const pending = stubPending();
	const controller = new AbortController();

	const abandoned = connect(url, { websocket: { enabled: false }, signal: controller.signal }).then(
		() => undefined,
		(err: unknown) => err,
	);
	const waiting = connect(url, { websocket: { enabled: false } }).then(
		() => undefined,
		(err: unknown) => err,
	);

	await settle();

	const reason = new Error("gave up");
	controller.abort(reason);
	expect(await abandoned).toBe(reason);

	await settle();
	// Somebody is still waiting on it, so the attempt is untouched.
	expect(pending.closes()).toBe(0);

	resetPool();
	await waiting;
});

test("the last caller aborting mid-connect drops the attempt immediately", async () => {
	const pending = stubPending();
	const controller = new AbortController();

	const abandoned = connect(url, { websocket: { enabled: false }, signal: controller.signal, pool: { grace } }).then(
		() => undefined,
		(err: unknown) => err,
	);

	await settle();
	controller.abort();
	await abandoned;
	await settle();

	// No linger: a half-open dial has nothing warm worth keeping.
	expect(pending.closes()).toBe(1);
});

test("a caller arriving after an abort starts a fresh attempt", async () => {
	const pending = stubPending();
	const controller = new AbortController();

	const abandoned = connect(url, { websocket: { enabled: false }, signal: controller.signal }).then(
		() => undefined,
		(err: unknown) => err,
	);

	await settle();
	controller.abort();
	await abandoned;

	const next = connect(url, { websocket: { enabled: false } }).catch(() => undefined);
	await settle();
	expect(pending.connects()).toBe(2);

	resetPool();
	await next;
});

test("pool: false gets a session of its own", async () => {
	const { transports } = stubTransports();

	const shared = await connect(url, { websocket: { enabled: false } });
	const alone = await connect(url, { websocket: { enabled: false }, pool: false });

	expect(transports.length).toBe(2);

	// Closing the dedicated session doesn't touch the shared one.
	let sharedClosed = false;
	void transports[0]?.closed.then(() => {
		sharedClosed = true;
	});
	alone.close();
	await settle();
	expect(sharedClosed).toBe(false);

	shared.close();
});

test("options that change the session are not shared", async () => {
	const { transports } = stubTransports();

	const first = await connect(url, { websocket: { enabled: false } });
	const second = await connect(url, { websocket: { enabled: false }, discovery: false });
	const third = await connect(url, {
		websocket: { enabled: false },
		webtransport: { serverCertificateHashes: [{ value: "ab" }] },
	});

	expect(transports.length).toBe(3);
	expect(second.discovery).toBe(false);

	first.close();
	second.close();
	third.close();
});

test("a supplied transport is never shared", async () => {
	const first = createMockTransportPair(ALPN_05);
	const second = createMockTransportPair(ALPN_05);

	const one = await connect(url, { transport: first.client });
	const two = await connect(url, { transport: second.client });
	expect(two).not.toBe(one);

	let closed = false;
	void second.client.closed.then(() => {
		closed = true;
	});

	one.close();
	await settle();
	expect(closed).toBe(false);

	two.close();
});

test("releasing a handle closes what it opened, not the session", async () => {
	const { transports } = stubTransports();

	const first = await connect(url, { websocket: { enabled: false } });
	const second = await connect(url, { websocket: { enabled: false } });

	const mine = first.consume(Path.from("mine"));
	const yours = second.consume(Path.from("yours"));
	const announced = first.announced();

	first.close();

	expect(mine.closed.peek()).not.toBeUndefined();
	expect(announced.closed.peek()).not.toBeUndefined();
	expect(yours.closed.peek()).toBeUndefined();

	let closed = false;
	void transports[0]?.closed.then(() => {
		closed = true;
	});
	await settle();
	expect(closed).toBe(false);

	yours.close();
	second.close();
});

test("a released handle refuses to hand out more", async () => {
	stubTransports();

	const connection = await connect(url, { websocket: { enabled: false } });
	connection.close();

	expect(() => connection.consume(Path.from("late"))).toThrow();
	expect(() => connection.announced()).toThrow();

	// Idempotent, so a second close doesn't double-release the session.
	connection.close();
});
