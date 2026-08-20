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
type Broadcast = import("@moq/wasm").Broadcast;
type Track = import("@moq/wasm").Track;
type Group = import("@moq/wasm").Group;

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

/** How often the fixture publisher appends a group. */
const GROUP_INTERVAL_MS = 50;

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

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

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
 * Publish {@link FIXTURE} on `path` for the duration of `run`, one group every
 * {@link GROUP_INTERVAL_MS}.
 *
 * It keeps publishing rather than writing a fixed number of groups and closing:
 * a track that ends before the subscriber's stream is wired up is only reachable
 * through the relay's cache, so the test would be measuring retention, and would
 * race. A live edge that keeps moving has neither problem. Every track name
 * other than {@link TRACK} is rejected, which is what the refusal case reads.
 */
async function withPublisher<T>(relay: RelayFixture, path: string, run: () => Promise<T>): Promise<T> {
	// The session serves an origin rather than individual broadcasts, so the path is
	// published into the origin and the session announces the table.
	const origin = new Moq.Origin.Producer();
	const broadcast = origin.publish(Moq.Path.from(path));
	const connection = await Moq.Connection.connect(new URL(relay.url), { publish: origin.consume() });

	let stopped = false;
	const writers: Promise<void>[] = [];

	// lite-05 looks a track's info up before subscribing, so the same name can be
	// requested more than once. Each request gets its own producer writing the same
	// groups; the subscriber only ever reads the one it asked for.
	const serving = (async () => {
		for (;;) {
			const request = await broadcast.requested();
			if (!request) break;
			if (request.name !== TRACK) {
				request.reject(new Error(`no such track: ${request.name}`));
				continue;
			}
			const track = request.accept();
			writers.push(
				(async () => {
					while (!stopped && track.closed.peek() === undefined) {
						const group = track.appendGroup();
						for (const frame of FIXTURE) {
							group.writeFrame({ payload: frame, timestamp: Moq.Time.Timestamp.now() });
						}
						group.close();
						await sleep(GROUP_INTERVAL_MS);
					}
					track.close();
				})(),
			);
		}
	})();

	try {
		return await run();
	} finally {
		stopped = true;
		broadcast.close();
		connection.close();
		origin.close();
		await serving;
		await Promise.all(writers);
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

/** Wait for `path` to be announced and return the broadcast, or fail. */
async function consume(session: Session, path: string): Promise<Broadcast> {
	const broadcast = await session.consume(path);
	if (!broadcast) throw new Error(`consume(${path}) resolved without a broadcast`);
	return broadcast;
}

/**
 * Read `count` groups off a track and check each one against {@link FIXTURE}.
 *
 * The first sequence is whatever the live edge was at subscribe time, so only
 * the step between groups is asserted, not the starting number.
 */
async function expectGroups(track: Track, count: number): Promise<void> {
	let previous: bigint | undefined;
	let read = 0;

	// A skip is legal (see `isOld`), so bound the attempts rather than the skips: a
	// subscription that never delivers a whole group fails here instead of looping.
	for (let attempt = 0; read < count; attempt++) {
		if (attempt >= count * 4) throw new Error(`read ${read} whole groups in ${attempt} attempts, want ${count}`);

		let group: Group | undefined;
		try {
			group = await track.recvGroup();
			if (!group) throw new Error(`track ended after ${read} groups, want ${count}`);

			// Forward, not consecutive: a skipped group leaves a gap in the sequence.
			if (previous !== undefined && group.sequence <= previous) {
				throw new Error(`group ${read}: sequence is ${group.sequence}, want greater than ${previous}`);
			}
			previous = group.sequence;

			for (const [j, want] of FIXTURE.entries()) {
				const frame = await group.readFrame();
				if (!frame) throw new Error(`group ${read}: ended after ${j} frames, want ${FIXTURE.length}`);
				expectBytes(frame, want, `group ${read} frame ${j}`);
			}
			if (await group.readFrame()) throw new Error(`group ${read}: got more than ${FIXTURE.length} frames`);
			read++;
		} catch (err) {
			if (!isOld(err)) throw err;
		} finally {
			group?.free();
		}
	}
}

/**
 * Whether the live edge passed this group before it was delivered whole.
 *
 * The subscription's budget is the default `Latency::REAL_TIME`, which drops a group
 * the edge has moved past rather than delivering it late, so the publisher writing one
 * every {@link GROUP_INTERVAL_MS} can legitimately retire a group the reader is still
 * on. Treating that as a case failure asserts a guarantee real-time delivery does not
 * make, and loses the race on a loaded runner. What has to hold is that a group which
 * *is* delivered arrives in order and whole, which the checks above still cover.
 */
function isOld(err: unknown): boolean {
	return err instanceof Error && err.message === "old";
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
			await withPublisher(relay, path, async () => {
				await withSession(wasm, relay, async (session) => {
					const broadcast = await consume(session, path);
					const track = await broadcast.subscribe(TRACK);
					try {
						await expectGroups(track, GROUPS_TO_READ);
					} finally {
						track.free();
						broadcast.free();
					}
				});
			});
		},
	},
	{
		// A refusal surfaces at different points depending on the version: lite-05
		// looks a track's info up before subscribing, so `subscribe` itself rejects,
		// while IETF and lite-02 have no such lookup and only find out when no group
		// arrives. Either is fine. Delivering a group is not, and neither is never
		// settling, which the case timeout catches.
		name: "refuses a track the publisher does not serve",
		run: async (wasm, relay) => {
			const path = `wasm-test/${relay.name}-refuse`;
			await withPublisher(relay, path, async () => {
				await withSession(wasm, relay, async (session) => {
					const broadcast = await consume(session, path);
					let delivered = false;
					try {
						const track = await broadcast.subscribe("missing");
						try {
							const group = await track.recvGroup();
							if (group) {
								delivered = true;
								group.free();
							}
						} finally {
							track.free();
						}
					} catch {
						// The refusal surfaced as a rejection, the other legal shape.
					} finally {
						broadcast.free();
					}
					if (delivered) throw new Error("an unserved track delivered a group");
				});
			});
		},
	},
];

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
