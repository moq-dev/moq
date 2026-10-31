/**
 * The in-page half of the `@moq/wasm` browser harness.
 *
 * Registers `moqWasmTest(config)` on the window; `driver.ts` calls it from
 * Playwright and reports what comes back. The publisher is `@moq/net` (the
 * hand-written TypeScript implementation) and the subscriber is `@moq/wasm`
 * (the Rust one compiled to WebAssembly), so every case is also a cross-
 * implementation interop check against a real relay.
 *
 * The wasm module is loaded through a URL handed in at runtime rather than an
 * `import` the bundler can see, so what runs here is `js/wasm/dist` exactly as
 * it ships: the generated glue, the `.wasm` it fetches, and the JS names
 * `#[wasm_bindgen]` chose. A binding that compiles but exports the wrong name
 * fails here and nowhere else.
 *
 * @module
 */
import * as Moq from "@moq/net";

/** The generated `@moq/wasm` module, typed from its published `.d.ts`. */
type Wasm = typeof import("@moq/wasm");
type Session = import("@moq/wasm").Session;
type Track = import("@moq/wasm").Track;

/** One relay the suite runs against, and the version it must negotiate. */
export interface RelayFixture {
	/** Label used in case names, e.g. "lite". */
	name: string;
	/** The relay's `http://` origin. Rewritten to `https://` for WebTransport. */
	url: string;
	/** What `Session.version()` has to report, e.g. "moq-lite-05". */
	version: string;
}

/** Everything the driver hands the page. */
export interface Config {
	/** URL of the generated `moq.js`, served next to this page. */
	module: string;
	/** The relays to run the suite against. */
	relays: RelayFixture[];
	/** Per-case budget in milliseconds. */
	timeout: number;
}

/** The outcome of a single case, as reported back to the driver. */
export interface CaseResult {
	/** `<relay>: <case>`, unique across the run. */
	name: string;
	/** True when the case passed. */
	ok: boolean;
	/** The failure message, when it didn't. */
	error?: string;
	/** Set when this failure is the known one for this relay, naming its issue. */
	known?: string;
}

/** The one track the fixture publisher serves. */
const TRACK = "data";

/**
 * Big enough to span several WebTransport read chunks, which is the only thing
 * that exercises the copy loop in the transport adapter's `read`.
 */
const LARGE_FRAME_BYTES = 128 * 1024;

/** The frames in every published group, in order. */
const FIXTURE: Uint8Array[] = [encode("wasm-frame-0"), encode("wasm-frame-1"), pattern(LARGE_FRAME_BYTES)];

/** How many consecutive groups the round trip reads before it is satisfied. */
const GROUPS_TO_READ = 2;

function encode(text: string): Uint8Array {
	return new TextEncoder().encode(text);
}

/** A deterministic byte pattern; 251 is prime, so it never aligns with a chunk boundary. */
function pattern(length: number): Uint8Array {
	const bytes = new Uint8Array(length);
	for (let i = 0; i < length; i++) bytes[i] = i % 251;
	return bytes;
}

function hexToBytes(hex: string): Uint8Array {
	if (hex.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(hex)) throw new Error(`not hex: ${hex}`);
	const bytes = new Uint8Array(hex.length / 2);
	for (let i = 0; i < bytes.length; i++) bytes[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
	return bytes;
}

/** Load and initialize the generated module once per page. */
let loading: Promise<Wasm> | undefined;
function load(url: string): Promise<Wasm> {
	loading ??= (async () => {
		// The specifier is a variable so the bundler leaves it alone; see the module doc.
		const wasm = (await import(url)) as Wasm;
		await wasm.default();
		wasm.setup(); // panic hook + tracing, so a Rust panic reaches the console
		return wasm;
	})();
	return loading;
}

/**
 * Connect the wasm bindings to a relay, pinning its self-signed certificate.
 *
 * `http://` can't be a WebTransport origin, so the relay's HTTP listener serves
 * the hash and the URL is rewritten, the same dance `@moq/net` does internally.
 */
async function connect(wasm: Wasm, relay: RelayFixture): Promise<Session> {
	const url = new URL(relay.url);
	const fingerprint = new URL(url);
	fingerprint.pathname = "/certificate.sha256";

	const response = await fetch(fingerprint);
	if (!response.ok) throw new Error(`${fingerprint}: HTTP ${response.status}`);
	const hash = hexToBytes((await response.text()).trim());

	url.protocol = "https:";
	return await wasm.Session.connectWithHashes(url.toString(), [hash]);
}

/**
 * Publish {@link FIXTURE} on `path` for the duration of `run`: one group once the
 * track has a subscriber, then one more per call to the `advance` handed to `run`.
 *
 * The reader paces the publisher, not a timer. The wasm subscription has the default
 * `Latency::REAL_TIME` budget, which gives up on a group still arriving once a newer
 * one lands. A loaded runner can deliver a 128 KiB group slower than any fixed
 * interval, so every group is superseded mid-read and none arrives whole. With no
 * newer group until the reader asks, whole delivery is guaranteed rather than raced.
 *
 * The track stays open rather than ending after a fixed number of groups: a track
 * that ends before the subscriber's stream is wired up is only reachable through
 * the relay's cache, so the test would be measuring retention, and would race. The
 * `missing` track is rejected when subscribed, which is what the refusal case reads.
 */
async function withPublisher<T>(
	relay: RelayFixture,
	path: string,
	run: (advance: () => void) => Promise<T>,
): Promise<T> {
	// The session serves an origin rather than individual broadcasts, so the path is
	// published into the origin and the session announces the table.
	const origin = new Moq.Origin.Producer();
	const broadcastPublication = origin.createBroadcast(Moq.Path.from(path));
	const broadcast = broadcastPublication.broadcast;
	const track = broadcast.createTrack(TRACK, { timescale: Moq.Time.Timescale.MILLI });
	const missing = broadcast.createTrack("missing");
	const used = track.demand().wanted;
	const missingUsed = missing.demand().wanted;
	broadcastPublication.announce();
	const connection = await Moq.Connection.connect({ url: new URL(relay.url), publish: origin.consume() });

	let stopped = false;
	let wanted = 1;
	let wake: (() => void) | undefined;
	const advance = () => {
		wanted++;
		wake?.();
	};
	const writer = (async () => {
		while (!stopped && track.closed.peek() === undefined) {
			if (used.peek() === undefined || wanted === 0) {
				const advanced = new Promise<void>((resolve) => {
					wake = resolve;
				});
				await Promise.race([used.changed(), track.closed, advanced]);
				continue;
			}
			wanted--;
			const group = track.appendGroup();
			for (const frame of FIXTURE) {
				group.writeFrame({ payload: frame, timestamp: Moq.Time.Timestamp.now() });
			}
			group.finish();
		}
		track.finish();
		track.close();
	})();
	const rejecting = (async () => {
		while (missingUsed.peek() === undefined && missing.closed.peek() === undefined) {
			await Promise.race([missingUsed.changed(), missing.closed]);
		}
		if (missingUsed.peek() !== undefined) missing.abort(new Error("no such track: missing"));
	})();

	try {
		return await run(advance);
	} finally {
		stopped = true;
		track.finish();
		track.close();
		missing.finish();
		missing.close();
		broadcast.close();
		// Teardown, not a graceful end: the case is over and the harness is not waiting
		// on delivery, so skip the drain `close()` would spend up to a second on.
		connection.abort();
		origin.close();
		await writer;
		await rejecting;
	}
}

/** Run `body` against a connected wasm session, freeing it afterwards. */
async function withSession<T>(wasm: Wasm, relay: RelayFixture, body: (session: Session) => Promise<T>): Promise<T> {
	const session = await connect(wasm, relay);
	try {
		return await body(session);
	} finally {
		// Dropping the Rust value closes the transport, which ends the driver task.
		session.free();
	}
}

/**
 * Read `count` consecutive groups off a track, asking the publisher for each next one
 * with `advance`, and check each against {@link FIXTURE}.
 *
 * Every group must arrive whole: nothing newer exists while one is read (see
 * {@link withPublisher}), so a skip or a short group is a bug, not real-time delivery.
 * The first sequence is whatever the live edge was at subscribe time, so only the
 * step between groups is asserted, not the starting number.
 */
async function expectGroups(track: Track, count: number, advance: () => void): Promise<void> {
	let previous: bigint | undefined;

	for (let i = 0; i < count; i++) {
		const group = await track.recvGroup();
		if (!group) throw new Error(`track ended after ${i} groups, want ${count}`);
		try {
			if (previous !== undefined && group.sequence !== previous + 1n) {
				throw new Error(`group ${i}: sequence is ${group.sequence}, want ${previous + 1n}`);
			}
			previous = group.sequence;

			for (const [j, want] of FIXTURE.entries()) {
				const frame = await group.readFrame();
				if (!frame) throw new Error(`group ${i}: ended after ${j} frames, want ${FIXTURE.length}`);
				expectBytes(frame, want, `group ${i} frame ${j}`);
			}
			if (await group.readFrame()) throw new Error(`group ${i}: got more than ${FIXTURE.length} frames`);
		} finally {
			group.free();
		}
		advance();
	}
}

function expectBytes(got: Uint8Array, want: Uint8Array, where: string): void {
	if (got.length !== want.length) throw new Error(`${where}: got ${got.length} bytes, want ${want.length}`);
	for (let i = 0; i < want.length; i++) {
		if (got[i] !== want[i]) throw new Error(`${where}: byte ${i} is ${got[i]}, want ${want[i]}`);
	}
}

/** An expected failure: the issue that tracks it, and how to recognize it. */
interface Known {
	/** The issue tracking the bug, e.g. "#2903". */
	issue: string;
	/**
	 * Substring the failure has to contain to count as this bug.
	 *
	 * Excusing a whole case by name would excuse every future failure of it too,
	 * which would quietly retire the coverage: an unrelated regression in
	 * `consume`, `subscribe`, or frame copying would land green under the same
	 * marker. Anything that doesn't match is reported as the new failure it is.
	 */
	error: string;
}

interface Case {
	name: string;
	/**
	 * Relay flavours where this case is expected to fail. The driver reports a
	 * matching failure as `known`, and reports a pass as a failure, so the marker
	 * has to be removed once the bug is fixed.
	 */
	known?: Record<string, Known>;
	run: (wasm: Wasm, relay: RelayFixture) => Promise<void>;
}

const CASES: Case[] = [
	{
		// The #2811 regression: without `with_protocols` the browser negotiates no
		// subprotocol at all and moq-net rejects the empty string as an unknown ALPN,
		// so this fails to connect rather than reporting the wrong version.
		name: "negotiates the relay's version",
		run: async (wasm, relay) => {
			await withSession(wasm, relay, async (session) => {
				const version = session.version();
				if (version !== relay.version) throw new Error(`version is "${version}", want "${relay.version}"`);
			});
		},
	},
	{
		name: "reads a published broadcast",
		run: async (wasm, relay) => {
			const path = `wasm-test/${relay.name}`;
			await withPublisher(relay, path, async (advance) => {
				await withSession(wasm, relay, async (session) => {
					const broadcast = await session.consume(path);
					const track = await broadcast.subscribe(TRACK);
					try {
						await expectGroups(track, GROUPS_TO_READ, advance);
					} finally {
						track.free();
						broadcast.free();
					}
				});
			});
		},
	},
	{
		// The refusal has to surface as an error. Where depends on the version: lite-06
		// looks a track's info up before subscribing, so `subscribe` rejects, while IETF
		// (REQUEST_ERROR, or PUBLISH_DONE after SUBSCRIBE_OK) and lite-02 can accept first
		// and reject the read. Delivering a group or ending cleanly is not a refusal, and
		// never settling fails on the case timeout.
		name: "refuses a track the publisher does not serve",
		run: async (wasm, relay) => {
			const path = `wasm-test/${relay.name}-refuse`;
			await withPublisher(relay, path, async () => {
				await withSession(wasm, relay, async (session) => {
					const broadcast = await session.consume(path);
					let read: string;
					try {
						const track = await broadcast.subscribe("missing");
						try {
							const group = await track.recvGroup();
							group?.free();
							read = group ? "delivered a group" : "ended cleanly";
						} finally {
							track.free();
						}
					} catch {
						return;
					} finally {
						broadcast.free();
					}
					throw new Error(`an unserved track ${read}, want a refusal`);
				});
			});
		},
	},
	{
		// Freeing a session closes it, so a call still waiting on it has to reject
		// rather than hang or take the page's wasm down with it.
		name: "free() rejects a pending consume",
		run: async (wasm, relay) => {
			const session = await connect(wasm, relay);
			const pending = session.consume(`wasm-test/${relay.name}-never-announced`);
			session.free();
			const resolved = await pending.then(
				(broadcast) => {
					broadcast.free();
					return true;
				},
				() => false,
			);
			if (resolved) throw new Error("consume resolved after the session was freed, want a rejection");
			await expectUsable(wasm, relay);
		},
	},
	{
		// Freeing a handle cancels its pending call, even one that could finish
		// right away, and releases what the call held; the page keeps working.
		name: "free() cancels a pending read",
		run: async (wasm, relay) => {
			const path = `wasm-test/${relay.name}-free`;
			await withPublisher(relay, path, async () => {
				await withSession(wasm, relay, async (session) => {
					const broadcast = await session.consume(path);
					const track = await broadcast.subscribe(TRACK);
					const pendingTrack = broadcast.subscribe(TRACK);
					broadcast.free();
					await expectCancelled("subscribe", pendingTrack, (track) => track.free());

					const group = await track.recvGroup();
					if (!group) throw new Error("track ended before its first group");
					const pendingGroup = track.recvGroup();
					track.free();
					await expectCancelled("recvGroup", pendingGroup, (group) => group?.free());

					const frame = group.readFrame();
					group.free();
					await expectCancelled("readFrame", frame, () => {});
				});
			});
			await expectUsable(wasm, relay);
		},
	},
];

/** Fail unless `pending` rejects, releasing whatever it resolved with instead. */
async function expectCancelled<T>(what: string, pending: Promise<T>, release: (value: T) => void): Promise<void> {
	const resolved = await pending.then(
		(value) => {
			release(value);
			return true;
		},
		() => false,
	);
	if (resolved) throw new Error(`${what} resolved after its handle was freed, want a rejection`);
}

/** Fail unless a fresh session still works, which a corrupted wasm heap would not. */
async function expectUsable(wasm: Wasm, relay: RelayFixture): Promise<void> {
	await withSession(wasm, relay, async (session) => {
		const version = session.version();
		if (version !== relay.version) throw new Error(`version is "${version}", want "${relay.version}"`);
	});
}

/** Run every case against every relay, sequentially, and report each outcome. */
async function run(config: Config): Promise<CaseResult[]> {
	const wasm = await load(config.module);
	const results: CaseResult[] = [];

	for (const relay of config.relays) {
		for (const example of CASES) {
			const name = `${relay.name}: ${example.name}`;
			const known = example.known?.[relay.name];
			let timer: ReturnType<typeof setTimeout> | undefined;
			try {
				await Promise.race([
					example.run(wasm, relay),
					new Promise<never>((_resolve, reject) => {
						timer = setTimeout(
							() => reject(new Error(`timed out after ${config.timeout}ms`)),
							config.timeout,
						);
					}),
				]);
				// A pass still carries the marker, so the driver can fail on it.
				results.push({ name, ok: true, ...(known ? { known: known.issue } : {}) });
			} catch (err) {
				const error = err instanceof Error ? err.message : String(err);
				const expected = known !== undefined && error.includes(known.error);
				results.push({ name, ok: false, error, ...(expected ? { known: known.issue } : {}) });
			} finally {
				clearTimeout(timer);
			}
		}
	}

	return results;
}

declare global {
	interface Window {
		/** Entry point the Playwright driver evaluates. */
		moqWasmTest: (config: Config) => Promise<CaseResult[]>;
	}
}

window.moqWasmTest = run;
