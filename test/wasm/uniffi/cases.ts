/**
 * The in-page half of the UniFFI comparison: moq-ffi bound by ubrn's wasm2 player.
 *
 * `driver.ts` loads this page twice, once normally and once under a strict CSP that
 * forbids `new Function`, which forces the player's interpreted dispatcher.
 *
 * @module
 */
import {
	MoqAnnounceRoute,
	MoqBroadcastProducer,
	MoqClient,
	MoqClientConfig,
	MoqClientTls,
	MoqConnectionPhase,
	type MoqConnectionState,
	type MoqGroupConsumer,
	type MoqGroupConsumerLike,
	type MoqSessionLike,
	type MoqTrackConsumer,
	MoqTrackInfo,
	type MoqTrackProducer,
	uniffiInitAsync,
} from "./dist/index.ts";

export interface Relay {
	name: string;
	url: string;
	version: string;
}

export interface CaseResult {
	name: string;
	ok: boolean;
	error?: string;
	/** What the case observed, when the behavior is reported rather than asserted. */
	note?: string;
}

export interface Bench {
	name: string;
	iterations: number;
	us_per_op: number;
}

export interface Report {
	/** Whether the page allows `new Function`, which the player's fast dispatchers need. */
	eval: boolean;
	init_ms: number;
	cases: CaseResult[];
	bench: Bench[];
}

/** Above 2^53, so a lossy `number` conversion anywhere changes it; the largest QUIC varint. */
const HIGH = [2n ** 53n + 1n, 2n ** 62n - 1n];

/** The largest timestamp, which lite-05/06 can still zigzag as a group's first frame. */
const MAX_TIMESTAMP = 2n ** 61n - 1n;

/** The ladder's timestamp for `sequence`, capped where a timestamp stops. */
const stamp = (sequence: bigint) => (sequence < MAX_TIMESTAMP ? sequence : MAX_TIMESTAMP);

/** Microsecond ticks, so `timestampUs` passes through unscaled. */
const TIMED = MoqTrackInfo.create({ timescale: 1_000_000n });

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

function payload(size: number): ArrayBuffer {
	const bytes = new Uint8Array(size);
	for (let i = 0; i < size; i++) bytes[i] = i % 251;
	return bytes.buffer;
}

/** Resolve `promise`, or reject once `ms` passes. */
async function within<T>(promise: Promise<T>, ms: number, what: string): Promise<T> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		return await Promise.race([
			promise,
			new Promise<never>((_, reject) => {
				timer = setTimeout(() => reject(new Error(`${what} did not settle in ${ms}ms`)), ms);
			}),
		]);
	} finally {
		clearTimeout(timer);
	}
}

/** True when `promise` is still pending after the event loop turns over a few times. */
/** Map how `promise` settles; a timeout still throws rather than reading as a rejection. */
function settle<T, R>(
	promise: Promise<T>,
	ms: number,
	what: string,
	resolved: (value: T) => R,
	rejected: (err: unknown) => R,
): Promise<R> {
	return within(promise.then(resolved, rejected), ms, what);
}

async function pending(promise: Promise<unknown>): Promise<boolean> {
	let settled = false;
	promise.then(
		() => {
			settled = true;
		},
		() => {
			settled = true;
		},
	);
	await sleep(20);
	return !settled;
}

/** Start a dial to `relay`, trusting `fingerprint` (the relay's own by default). */
async function dial(relay: Relay, fingerprint?: string): Promise<MoqSessionLike> {
	if (fingerprint === undefined) {
		const response = await fetch(new URL("/certificate.sha256", relay.url));
		if (!response.ok) throw new Error(`certificate HTTP ${response.status}`);
		fingerprint = (await response.text()).trim();
	}
	const url = new URL(relay.url);
	url.protocol = "https:";
	const client = new MoqClient(
		MoqClientConfig.create({
			// Offering only the expected version makes connecting the check.
			versions: [relay.version],
			tls: MoqClientTls.create({ fingerprints: [fingerprint] }),
		}),
	);
	return client.connect(url.toString());
}

/** Wait for the dial to settle, returning its state. */
async function settled(session: MoqSessionLike): Promise<MoqConnectionState> {
	let state = session.state();
	while (state.phase === MoqConnectionPhase.Connecting) {
		state = await within(session.stateChanged(state.revision), 10000, "dial");
	}
	return state;
}

/** Connect to `relay`, failing unless the session establishes. */
async function connect(relay: Relay): Promise<MoqSessionLike> {
	const session = await dial(relay);
	const state = await settled(session);
	if (state.phase !== MoqConnectionPhase.Connected) throw new Error(`dial ended: ${state.failure}`);
	return session;
}

/** A standalone group with a consumer, no session. */
function localTrack() {
	const broadcast = new MoqBroadcastProducer();
	const track = broadcast.publishTrack("data", TIMED);
	return {
		broadcast,
		track,
		// A child handle does not own its producer's lifetime. Retain both parents until
		// the case completes, then release them even when an assertion throws.
		[Symbol.dispose]() {
			(track as MoqTrackProducer).uniffiDestroy();
			broadcast.uniffiDestroy();
		},
	};
}

function localGroup(sequence = 0n) {
	const fixture = localTrack();
	const group = fixture.track.createGroup(sequence);
	return { ...fixture, group, consumer: group.consume() };
}

const local: Record<string, () => Promise<string | undefined>> = {
	"u64 above 2^53 round-trips locally": async () => {
		for (const sequence of HIGH) {
			using fixture = localGroup(sequence);
			const { group, consumer } = fixture;
			if (consumer.sequence() !== sequence) throw new Error(`sequence ${consumer.sequence()} != ${sequence}`);
			group.writeFrame({ payload: payload(1), timestampUs: stamp(sequence) });
			const frame = await consumer.readFrame();
			if (frame?.timestampUs !== stamp(sequence))
				throw new Error(`timestamp ${frame?.timestampUs} != ${stamp(sequence)}`);
		}
		return undefined;
	},

	"writeFrame refuses a timestamp past 2^61 - 1": async () => {
		using fixture = localGroup();
		const { group } = fixture;
		try {
			group.writeFrame({ payload: payload(1), timestampUs: MAX_TIMESTAMP + 1n });
		} catch (err) {
			return (err as Error).message;
		}
		throw new Error("an unencodable timestamp was written");
	},

	"AbortSignal rejects a pending read without stealing the next frame": async () => {
		using fixture = localGroup();
		const { group, consumer } = fixture;
		const abort = new AbortController();
		const first = consumer.readFrame({ signal: abort.signal });
		if (!(await pending(first))) throw new Error("read settled with nothing written");
		abort.abort();
		const error = await settle(
			first,
			1000,
			"aborted read",
			() => undefined,
			(err: unknown) => err,
		);
		if (!error) throw new Error("aborted read resolved");
		group.writeFrame({ payload: payload(3), timestampUs: 0n });
		const next = await within(consumer.readFrame(), 1000, "read after abort");
		if (next?.payload.byteLength !== 3) throw new Error("the frame after an abort went missing");
		return `rejects with ${(error as Error).name}: ${(error as Error).message}`;
	},

	"aborting a pending call releases its handle": async () => {
		// The pending future holds a clone of the consumer, so the track stays demanded until
		// rust_future_free drops it. Destroying the JS handle alone must not be enough.
		using fixture = localTrack();
		const { track } = fixture;
		const demand = track.demand();
		const consumer = track.consume(undefined);
		const wanted = async () => {
			while ((await demand.wantedChanged()) === undefined) {}
		};
		const unwanted = async () => {
			while ((await demand.wantedChanged()) !== undefined) {}
		};
		await within(wanted(), 1000, "demand wanted");
		const abort = new AbortController();
		const read = consumer.recvGroup({ signal: abort.signal }).catch(() => undefined);
		// The generated `*Like` return types omit uniffiDestroy; only the class declares it.
		(consumer as MoqTrackConsumer).uniffiDestroy();
		await sleep(20);
		const heldByFuture = demand.wanted() !== undefined;
		abort.abort();
		await read;
		await within(unwanted(), 1000, "demand unwanted after abort");
		if (!heldByFuture) throw new Error("demand dropped before the pending future was freed");
		return undefined;
	},

	"freeing a handle mid-call leaves the call running": async () => {
		// moq-wasm's free() rejects pending calls. Here every call clones the Arc, so the
		// call outlives the handle and cancel() is the explicit way to end it.
		using fixture = localGroup();
		const { group, consumer } = fixture;
		const read = consumer.readFrame();
		(consumer as MoqGroupConsumer).uniffiDestroy();
		if (!(await pending(read))) throw new Error("free settled the pending read");
		group.writeFrame({ payload: payload(2), timestampUs: 0n });
		const frame = await within(read, 1000, "read after free");
		return `resolves later with ${frame?.payload.byteLength} bytes`;
	},

	"cancel() rejects a pending call and every later one": async () => {
		using fixture = localGroup();
		const { consumer } = fixture;
		const read = consumer.readFrame();
		if (!(await pending(read))) throw new Error("read settled with nothing written");
		consumer.cancel();
		const first = await settle(
			read,
			1000,
			"cancelled read",
			() => undefined,
			(err: unknown) => err as Error,
		);
		if (!first) throw new Error("cancelled read resolved");
		const later = await settle(
			consumer.readFrame(),
			1000,
			"read after cancel",
			() => undefined,
			(err: unknown) => err as Error,
		);
		if (!later) throw new Error("read after cancel resolved");
		return `${first.message}; then ${later.message}`;
	},

	"closing the producer ends a pending read": async () => {
		using fixture = localGroup();
		const { group, consumer } = fixture;
		const read = consumer.readFrame();
		group.finish();
		const frame = await within(read, 1000, "read after finish");
		if (frame !== undefined) throw new Error("finished group yielded a frame");
		return undefined;
	},
};

const remote: Record<string, (relay: Relay) => Promise<string | undefined>> = {
	"negotiates the relay's version": async (relay) => {
		const session = await connect(relay);
		const state = session.state();
		session.cancel(0);
		if (state.version !== relay.version) throw new Error(`negotiated ${state.version}`);
		if (state.connects !== 1n) throw new Error(`connects ${state.connects}`);
		return undefined;
	},

	"connect returns before a refused dial and reports it": async (relay) => {
		// A certificate hash nothing matches, so the browser refuses the handshake.
		const session = await dial(relay, "00".repeat(32));
		const first = session.state();
		if (first.phase !== MoqConnectionPhase.Connecting) throw new Error(`returned ${first.phase}`);
		const state = await settled(session);
		if (state.phase !== MoqConnectionPhase.Closed) throw new Error(`settled ${state.phase}`);
		if (!state.failure) throw new Error("no failure reported");
		const closed = await settle(
			session.closed(),
			5000,
			"closed",
			() => "resolved",
			(err: unknown) => (err as Error).message,
		);
		if (closed === "resolved") throw new Error("closed resolved after a refused dial");
		return closed;
	},

	"cancelling a dial closes it": async (relay) => {
		const session = await dial(relay);
		session.cancel(0);
		const state = await settled(session);
		if (state.phase !== MoqConnectionPhase.Closed) throw new Error(`settled ${state.phase}`);
		await within(session.closed(), 5000, "closed");
		return undefined;
	},

	"u64 above 2^53 crosses the relay": async (relay) => {
		const publisher = await connect(relay);
		const subscriber = await connect(relay);
		try {
			const path = `uniffi/u64/${crypto.randomUUID()}`;
			const publication = publisher.publish().createBroadcast(path);
			const broadcast = publication.broadcast();
			const track = broadcast.publishTrack("data", TIMED);
			publication.announce(MoqAnnounceRoute.create({}));
			const consumer = await within(subscriber.consume().announcedBroadcast(path).available(), 5000, "announce");
			const sub = await consumer.subscribeTrack("data", undefined);
			const seen: string[] = [];
			for (const sequence of [5n, 2n ** 32n, 2n ** 53n - 1n, ...HIGH]) {
				const group = track.createGroup(sequence);
				group.writeFrame({ payload: payload(160), timestampUs: stamp(sequence) });
				group.finish();
				const got = (await within(sub.recvGroup(), 5000, `recvGroup ${sequence} after [${seen}]`)) as
					| MoqGroupConsumerLike
					| undefined;
				if (got?.sequence() !== sequence) throw new Error(`sequence ${got?.sequence()} != ${sequence}`);
				const frame = await got.readFrame();
				// Lite-02 carries no timestamp and a lite-05/06 subscriber drops it; the rest keep it exact.
				const expected = /^moq-lite-0[1-6]$/.test(relay.version) ? undefined : stamp(sequence);
				if (frame?.timestampUs !== expected) throw new Error(`timestamp ${frame?.timestampUs} != ${expected}`);
				seen.push(String(sequence));
			}
			return undefined;
		} finally {
			publisher.cancel(0);
			subscriber.cancel(0);
		}
	},

	"refuses a track the publisher does not serve": async (relay) => {
		const publisher = await connect(relay);
		const subscriber = await connect(relay);
		try {
			const path = `uniffi/refuse/${crypto.randomUUID()}`;
			const publication = publisher.publish().createBroadcast(path);
			const broadcast = publication.broadcast();
			broadcast.publishTrack("data", TIMED);
			publication.announce(MoqAnnounceRoute.create({}));
			const consumer = await within(subscriber.consume().announcedBroadcast(path).available(), 5000, "announce");
			const outcome = await settle(
				consumer.subscribeTrack("missing", undefined).then((track) => track.recvGroup()),
				5000,
				"refused subscribe",
				(group) => (group ? "a group" : "no group"),
				(err: unknown) => `rejected: ${(err as Error).message}`,
			);
			if (outcome === "a group") throw new Error("a missing track yielded a group");
			return outcome;
		} finally {
			publisher.cancel(0);
			subscriber.cancel(0);
		}
	},

	"session cancel rejects a pending remote read": async (relay) => {
		const publisher = await connect(relay);
		const subscriber = await connect(relay);
		try {
			const path = `uniffi/close/${crypto.randomUUID()}`;
			const publication = publisher.publish().createBroadcast(path);
			const broadcast = publication.broadcast();
			const track = broadcast.publishTrack("data", TIMED);
			publication.announce(MoqAnnounceRoute.create({}));
			const consumer = await within(subscriber.consume().announcedBroadcast(path).available(), 5000, "announce");
			const sub = await consumer.subscribeTrack("data", undefined);
			const group = track.appendGroup();
			group.writeFrame({ payload: payload(4), timestampUs: 0n });
			const got = await within(sub.recvGroup(), 5000, "recvGroup");
			if (!got) throw new Error("no group");
			await got.readFrame();
			const read = got.readFrame();
			if (!(await pending(read))) throw new Error("read settled early");
			subscriber.cancel(1);
			const outcome = await settle(
				read,
				5000,
				"read after session cancel",
				(frame) => (frame ? "a frame" : "end of group"),
				(err: unknown) => `rejected: ${(err as Error).message}`,
			);
			if (outcome === "a frame") throw new Error("read yielded a frame after cancel");
			return outcome;
		} finally {
			publisher.cancel(0);
		}
	},
};

/** Per-call costs with no network, so only the binding and moq-net's model are measured. */
async function bench(): Promise<Bench[]> {
	const results: Bench[] = [];
	// Each batch is a fresh group, under moq-net's 8192-frame and 32 MiB group caps.
	let BATCH = 1000;
	const measure = async (name: string, batches: number, body: () => Promise<number>) => {
		await body(); // warm the dispatchers
		let elapsed = 0;
		for (let i = 0; i < batches; i++) elapsed += await body();
		results.push({ name, iterations: batches * BATCH, us_per_op: (elapsed * 1000) / (batches * BATCH) });
	};

	await measure("sync getter: group.sequence()", 10, async () => {
		using fixture = localGroup();
		const { consumer } = fixture;
		const start = performance.now();
		for (let i = 0; i < BATCH; i++) consumer.sequence();
		return performance.now() - start;
	});

	for (const size of [160, 4096, 131072]) {
		const bytes = payload(size);
		BATCH = Math.min(1000, (16 << 20) / size);
		const batches = size > 4096 ? 8 : 10;
		await measure(`sync write_frame ${size}B`, batches, async () => {
			using fixture = localGroup();
			const { group, broadcast } = fixture;
			const start = performance.now();
			for (let i = 0; i < BATCH; i++) group.writeFrame({ payload: bytes, timestampUs: 0n });
			const elapsed = performance.now() - start;
			broadcast.close();
			return elapsed;
		});
		await measure(`async read_frame, buffered ${size}B`, batches, async () => {
			using fixture = localGroup();
			const { group, consumer, broadcast } = fixture;
			for (let i = 0; i < BATCH; i++) group.writeFrame({ payload: bytes, timestampUs: 0n });
			const start = performance.now();
			for (let i = 0; i < BATCH; i++) await consumer.readFrame();
			const elapsed = performance.now() - start;
			broadcast.close();
			return elapsed;
		});
	}

	BATCH = 1000;
	// Each read parks first, so it pays the continuation: a poll, a JS Promise per wakeup, a re-poll.
	const bytes = payload(160);
	await measure("async read_frame, parked then woken 160B", 5, async () => {
		using fixture = localGroup();
		const { group, consumer, broadcast } = fixture;
		const start = performance.now();
		for (let i = 0; i < BATCH; i++) {
			const read = consumer.readFrame();
			group.writeFrame({ payload: bytes, timestampUs: 0n });
			await read;
		}
		const elapsed = performance.now() - start;
		broadcast.close();
		return elapsed;
	});
	return results;
}

async function run(relays: Relay[]): Promise<Report> {
	const start = performance.now();
	// The page sets the URL; under the strict CSP this also probes `new Function` and falls back.
	await uniffiInitAsync(new URL("/uniffi/moq_ffi.wasm", location.href));
	const init_ms = performance.now() - start;

	const cases: CaseResult[] = [];
	const record = async (name: string, body: () => Promise<string | undefined>) => {
		try {
			const note = await within(body(), 15000, name);
			cases.push({ name, ok: true, note });
		} catch (err) {
			cases.push({ name, ok: false, error: err instanceof Error ? err.message : String(err) });
		}
	};
	for (const [name, body] of Object.entries(local)) await record(name, body);
	for (const relay of relays)
		for (const [name, body] of Object.entries(remote)) await record(`${relay.name}: ${name}`, () => body(relay));

	let evaluates = true;
	try {
		new Function("");
	} catch {
		evaluates = false;
	}
	return { eval: evaluates, init_ms, cases, bench: await bench() };
}

declare global {
	interface Window {
		uniffiCases: typeof run;
	}
}
window.uniffiCases = run;
