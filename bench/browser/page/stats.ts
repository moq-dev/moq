/**
 * Connections held open over a real WebTransport (or qmux) session, to measure what local
 * transport-stat sampling costs while they idle, publish, receive, or linger in the pool.
 *
 * Every connection is a `Moq.Connection` handle, the reconnect loop applications hold. The
 * receive shape's publisher dials once with `Moq.Connection.connect`, which runs no loop, so it
 * adds the same delivery work to every variant without sampling of its own.
 */
import { Milli } from "@moq/clock";
import * as Moq from "@moq/net";
import Session from "@moq/qmux";
import { check, payload } from "../fixture.ts";
import "./observe.ts";

/** One sampling workload. */
export interface Sampling {
	url: string;
	path: string;
	/**
	 * `private`: distinct reconnect loops. `receive`: distinct loops each playing one track.
	 * `linger`: distinct pooled connections whose last handle closed, inside the linger window.
	 * `pooled`: identical handles sharing one pooled connection, the control.
	 * `qmux`: distinct loops over the WebSocket fallback.
	 */
	mode: "private" | "receive" | "linger" | "pooled" | "qmux";
	/** Distinct connections, or handles on the one connection for `pooled`. */
	connections: number;
	/** Reservations held on each distinct connection's allocator. */
	reservations: number;
}

/** What the window left behind, so a variant that stopped working cannot read as cheaper. */
export interface Outcome {
	connected: number;
	/** Reservations reporting a send-rate slice at the end of the window. */
	estimated: number;
	/** Frames the receive shape read during the whole run, every byte checked. */
	received: number;
	/** The transport the connections negotiated. */
	transport: string;
}

/** Calls the page made, counted by `calls.ts` wrappers installed before any module ran. */
export interface Counts {
	getstats: number;
	timers: number;
}

const RECEIVE_FPS = 30;
const RECEIVE_BYTES = 1200;

function counters(): Counts {
	const counts = (globalThis as { __sampling?: Counts }).__sampling;
	if (!counts) throw new Error("sampling counters are not installed");
	return counts;
}

// The qmux session's own getStats, which reports nothing, counted alongside WebTransport's.
{
	const proto = Session.prototype as unknown as { getStats: () => Promise<unknown> };
	const getStats = proto.getStats;
	proto.getStats = function (this: Session) {
		const counts = (globalThis as { __sampling?: Counts }).__sampling;
		if (counts) counts.getstats++;
		return getStats.call(this);
	};
}

const cleanup: (() => void)[] = [];
let handles: Moq.Connection[] = [];
let reservations: Moq.Bandwidth.Reservation[] = [];
let received = 0;
let readers: Promise<void>[] = [];
let lingering = false;

async function connected(connection: Moq.Connection): Promise<void> {
	while (connection.status.peek() !== "connected") {
		const error = connection.error.peek();
		if (error) throw error;
		await connection.status.changed();
	}
}

async function allocator(connection: Moq.Connection): Promise<Moq.Bandwidth.Handle> {
	for (;;) {
		const handle = connection.allocator.peek();
		if (handle) return handle;
		await connection.allocator.changed();
	}
}

/** A reservation on `handle` for a fresh, unannounced track: the allocator sees only its demand. */
function reserve(handle: Moq.Bandwidth.Handle, origin: Moq.Origin.Producer, name: string) {
	const publication = origin.createBroadcast(Moq.Path.from(name));
	const track = publication.broadcast.createTrack("video", { priority: 1 });
	const reservation = handle.reserve(track.demand(), 2_500_000);
	cleanup.push(() => {
		reservation.close();
		track.close();
		publication.broadcast.close();
	});
	return reservation;
}

/** Opens every connection, waits until each is connected, and leaves the shape's steady state. */
async function setup(config: Sampling): Promise<void> {
	const origin = new Moq.Origin.Producer();
	cleanup.push(() => origin.close());
	const url = (index: number) => {
		const url = new URL(config.url);
		// Distinct pool keys for linger; private loops ignore sharing anyway.
		if (config.mode === "linger") url.searchParams.set("connection", String(index));
		return url;
	};
	for (let index = 0; index < config.connections; index++) {
		const shared = config.mode === "linger" || config.mode === "pooled";
		const connection = new Moq.Connection({
			url: config.mode === "pooled" ? url(0) : url(index),
			share: shared ? undefined : false,
			// Any WebSocket option forces a private loop, so pooled shapes take the defaults.
			websocket: config.mode === "private" || config.mode === "receive" ? { enabled: false } : undefined,
		});
		handles.push(connection);
		cleanup.push(() => connection.close());
	}
	await Promise.all(handles.map(connected));
	for (const connection of handles) {
		const expected = config.mode === "qmux" ? "websocket" : "webtransport";
		if (connection.transport.peek() !== expected)
			throw new Error(`negotiated ${connection.transport.peek()}, expected ${expected}`);
	}

	// One allocator per distinct connection: pooled handles all lend the same one.
	const distinct = config.mode === "pooled" ? handles.slice(0, 1) : handles;
	for (const [index, connection] of distinct.entries()) {
		const handle = await allocator(connection);
		for (let slot = 0; slot < config.reservations; slot++)
			reservations.push(reserve(handle, origin, `${config.path}/reserve/${index}/${slot}`));
	}

	if (config.mode === "receive") await receive(config);
	if (config.mode === "linger") {
		// The handles go; the pooled connections stay up for the linger window.
		for (const connection of handles) connection.close();
		lingering = true;
	}
}

/** Publishes one small-frame track and plays it on every connection. */
async function receive(config: Sampling): Promise<void> {
	const publishing = new Moq.Origin.Producer();
	const publication = publishing.createBroadcast(Moq.Path.from(`${config.path}/live`));
	const track = publication.broadcast.createTrack("data", { timescale: Moq.Time.Timescale.MILLI });
	publication.announce();
	const publisher = await Moq.Connection.connect({
		url: new URL(config.url),
		publish: publishing.consume(),
		websocket: { enabled: false },
	});
	let frame = 0;
	const timer = setInterval(() => {
		const group = track.appendGroup();
		group.writeFrame({ payload: payload(RECEIVE_BYTES, frame), timestamp: Moq.Time.Timestamp.fromMillis(frame) });
		group.finish();
		frame++;
	}, 1000 / RECEIVE_FPS);
	cleanup.push(() => {
		clearInterval(timer);
		track.close();
		publication.broadcast.close();
		publisher.abort();
		publishing.close();
	});

	for (const connection of handles) {
		const origin = await originOf(connection);
		const request = origin.follow(Moq.Path.from(`${config.path}/live`));
		cleanup.push(() => request.close());
		const broadcast = await active(request);
		const subscriber = broadcast.track("data").subscribe({ maxDelay: Milli(1000) });
		cleanup.push(() => subscriber.close());
		readers.push(
			(async () => {
				for (;;) {
					const group = await subscriber.recvGroup();
					if (!group) return;
					try {
						const got = await group.readFrame();
						if (!got) continue;
						const index = new DataView(got.payload.buffer, got.payload.byteOffset, 4).getUint32(0, true);
						check(got.payload, RECEIVE_BYTES, index, true);
						received++;
					} finally {
						group.close();
					}
				}
			})(),
		);
	}
	// The window starts once every connection is playing.
	const want = received + handles.length;
	while (received < want) await new Promise((resolve) => setTimeout(resolve, 10));
}

async function originOf(connection: Moq.Connection): Promise<Moq.Origin.Table> {
	for (;;) {
		const origin = connection.origin.peek();
		if (origin) return origin;
		await connection.origin.changed();
	}
}

async function active(request: Moq.Origin.Requesting): Promise<Moq.Broadcast.Consumer> {
	for (;;) {
		const broadcast = request.active.peek();
		if (broadcast) return broadcast;
		const closed = request.closed.peek();
		if (closed !== undefined) throw closed ?? new Error("broadcast request closed");
		await Promise.race([request.active.changed(), request.closed]);
	}
}

function counts(): Counts {
	return { ...counters() };
}

function outcome(): Outcome {
	return {
		// A closed handle no longer reports; the pooled connection behind it is still counted live.
		connected: lingering ? handles.length : handles.filter((h) => h.status.peek() === "connected").length,
		estimated: reservations.filter((reservation) => reservation.peek() !== undefined).length,
		received,
		transport: handles[0]?.transport.peek() ?? "none",
	};
}

/**
 * Releases and re-takes one reservation `cycles` times, `idle_ms` apart, timing how long each
 * new reservation waits for its first send-rate slice.
 *
 * `undefined` tells a sender to hold its rate, so this is the latency until publishing that
 * resumes on an idle connection can follow the estimate again.
 */
async function resume(config: Sampling & { cycles: number; idle_ms: number; limit_ms: number }) {
	await setup({ ...config, reservations: 0 });
	const handle = await allocator(handles[0]);
	const origin = new Moq.Origin.Producer();
	cleanup.push(() => origin.close());
	const waits: number[] = [];
	for (let cycle = 0; cycle < config.cycles; cycle++) {
		await new Promise((resolve) => setTimeout(resolve, config.idle_ms));
		const publication = origin.createBroadcast(Moq.Path.from(`${config.path}/resume/${cycle}`));
		const track = publication.broadcast.createTrack("video", { priority: 1 });
		const start = performance.now();
		const reservation = handle.reserve(track.demand(), 2_500_000);
		const deadline = start + config.limit_ms;
		while (reservation.peek() === undefined && performance.now() < deadline)
			await Promise.race([
				reservation.rate.changed(),
				new Promise((resolve) => setTimeout(resolve, deadline - performance.now())),
			]);
		const rate = reservation.peek();
		waits.push(rate === undefined ? Number.NaN : performance.now() - start);
		reservation.close();
		track.close();
		publication.broadcast.close();
	}
	return waits;
}

async function stop(): Promise<void> {
	for (const close of cleanup.splice(0).reverse()) close();
	await Promise.allSettled(readers);
	handles = [];
	reservations = [];
	readers = [];
	received = 0;
	lingering = false;
}

declare global {
	interface Window {
		stats: {
			setup: typeof setup;
			counts: typeof counts;
			outcome: typeof outcome;
			resume: typeof resume;
			stop: typeof stop;
		};
	}
}
window.stats = { setup, counts, outcome, resume, stop };
