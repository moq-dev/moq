/**
 * Parity cases for the slice, identical for each generator: the subset of
 * `uniffi/cases.ts` the slice's surface can express.
 *
 * @module
 */
import type { Open, Session, Slice } from "./adapter.ts";

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

export interface Report {
	generator: string;
	/** Whether the page allows `new Function`. */
	eval: boolean;
	cases: CaseResult[];
}

/** Above 2^53, so a lossy `number` conversion anywhere changes it; the largest QUIC varint. */
const HIGH = [2n ** 53n + 1n, 2n ** 62n - 1n];

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

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

/** An error's name and message, plus any structured fields a generator attaches. */
function describe(err: unknown): string {
	if (!(err instanceof Error)) return String(err);
	const fields = JSON.stringify(err, (_, value) => (typeof value === "bigint" ? String(value) : value));
	return `${err.name}: ${err.message}${fields && fields !== "{}" ? ` ${fields}` : ""}`.slice(0, 300);
}

async function connect(slice: Slice, relay: Relay): Promise<Session> {
	const response = await fetch(new URL("/certificate.sha256", relay.url));
	if (!response.ok) throw new Error(`certificate HTTP ${response.status}`);
	const url = new URL(relay.url);
	url.protocol = "https:";
	return slice.connect(url.toString(), (await response.text()).trim(), relay.version);
}

const local: Record<string, (slice: Slice) => Promise<string | undefined>> = {
	"u64 above 2^53 round-trips locally": async (slice) => {
		for (const sequence of HIGH) {
			const group = slice.group(sequence);
			if (group.sequence() !== sequence) throw new Error(`sequence ${group.sequence()} != ${sequence}`);
			group.write(new Uint8Array(1), sequence);
			const frame = await group.read();
			if (frame?.timestampUs !== sequence) throw new Error(`timestamp ${frame?.timestampUs} != ${sequence}`);
		}
		return undefined;
	},

	"AbortSignal rejects a pending read without stealing the next frame": async (slice) => {
		const group = slice.group();
		const abort = new AbortController();
		const first = group.read(abort.signal);
		if (!(await pending(first))) throw new Error("read settled with nothing written");
		abort.abort();
		const error = await settle(
			first,
			1000,
			"aborted read",
			() => undefined,
			(err: unknown) => err ?? new Error("rejected with nothing"),
		);
		if (!error) throw new Error("aborted read resolved");
		group.write(new Uint8Array(3));
		const next = await within(group.read(), 1000, "read after abort");
		if (next?.payload.byteLength !== 3) throw new Error("the frame after an abort went missing");
		return `rejects with ${describe(error)}`;
	},

	"freeing a handle mid-call": async (slice) => {
		// Reported, not asserted: moq-wasm rejects the call, UniFFI keeps it running.
		const group = slice.group();
		const read = group.read();
		group.free();
		if (!(await pending(read))) {
			const outcome = await read.then(
				() => "resolves",
				(err: unknown) => `rejects with ${describe(err)}`,
			);
			return `free settles the pending read: ${outcome}`;
		}
		group.write(new Uint8Array(2));
		const outcome = await within(
			read.then(
				(frame) => `resolves later with ${frame?.payload.byteLength} bytes`,
				(err: unknown) => `rejects later with ${describe(err)}`,
			),
			1000,
			"read after free",
		);
		return `the call keeps running: ${outcome}`;
	},

	"closing the producer ends a pending read": async (slice) => {
		const group = slice.group();
		const read = group.read();
		group.close();
		const outcome = await within(
			read.then(
				(frame) => (frame ? "a frame" : "end of group"),
				(err: unknown) => `rejects with ${describe(err)}`,
			),
			1000,
			"read after close",
		);
		if (outcome === "a frame") throw new Error("closed group yielded a frame");
		return outcome;
	},
};

const remote: Record<string, (slice: Slice, relay: Relay) => Promise<string | undefined>> = {
	"negotiates the relay's version": async (slice, relay) => {
		(await connect(slice, relay)).cancel();
		return undefined;
	},

	"u64 above 2^53 crosses the relay": async (slice, relay) => {
		const publisher = await connect(slice, relay);
		const subscriber = await connect(slice, relay);
		try {
			const path = `slice/u64/${crypto.randomUUID()}`;
			const publish = publisher.publish(path);
			const track = await (await within(subscriber.broadcast(path), 5000, "announce")).subscribe("data");
			const seen: string[] = [];
			for (const sequence of [5n, 2n ** 32n, 2n ** 53n - 1n, ...HIGH]) {
				const group = publish.group(sequence);
				group.write(new Uint8Array(160), sequence);
				group.finish();
				const next = within(track.recvGroup(), 5000, `recvGroup ${sequence} after [${seen}]`);
				// Known: on moq-lite-06 the largest varint never arrives, as with moq-ffi.
				if (sequence === HIGH[1] && relay.version === "moq-lite-06") {
					const outcome = await next.then(
						(got) => `arrived as ${got.sequence()}`,
						(err: unknown) => describe(err),
					);
					if (outcome === `arrived as ${sequence}`)
						throw new Error("known lite-06 gap now passes; update the case");
					return `known gap: 2^62-1 ${outcome}`;
				}
				const got = await next;
				if (got.sequence() !== sequence) throw new Error(`sequence ${got.sequence()} != ${sequence}`);
				const frame = await got.read();
				// moq-lite-02 carries no timestamp; every other version must keep it exact.
				if (relay.version !== "moq-lite-02" && frame?.timestampUs !== sequence)
					throw new Error(`timestamp ${frame?.timestampUs} != ${sequence}`);
				seen.push(String(sequence));
			}
			return undefined;
		} finally {
			publisher.cancel();
			subscriber.cancel();
		}
	},

	"refuses a track the publisher does not serve": async (slice, relay) => {
		const publisher = await connect(slice, relay);
		const subscriber = await connect(slice, relay);
		try {
			const path = `slice/refuse/${crypto.randomUUID()}`;
			publisher.publish(path);
			const broadcast = await within(subscriber.broadcast(path), 5000, "announce");
			const outcome = await settle(
				broadcast.subscribe("missing").then((track) => track.recvGroup()),
				5000,
				"refused subscribe",
				() => "a group",
				(err: unknown) => `rejected: ${describe(err)}`,
			);
			if (outcome === "a group") throw new Error("a missing track yielded a group");
			return outcome;
		} finally {
			publisher.cancel();
			subscriber.cancel();
		}
	},

	"session cancel rejects a pending remote read": async (slice, relay) => {
		const publisher = await connect(slice, relay);
		const subscriber = await connect(slice, relay);
		try {
			const path = `slice/close/${crypto.randomUUID()}`;
			const publish = publisher.publish(path);
			const track = await (await within(subscriber.broadcast(path), 5000, "announce")).subscribe("data");
			const group = publish.group(0n);
			group.write(new Uint8Array(4), 0n);
			const got = await within(track.recvGroup(), 5000, "recvGroup");
			await got.read();
			const read = got.read();
			if (!(await pending(read))) throw new Error("read settled early");
			subscriber.cancel();
			const outcome = await settle(
				read,
				5000,
				"read after session cancel",
				(frame) => (frame ? "a frame" : "end of group"),
				(err: unknown) => `rejected: ${describe(err)}`,
			);
			if (outcome === "a frame") throw new Error("read yielded a frame after cancel");
			return outcome;
		} finally {
			publisher.cancel();
		}
	},
};

async function run(generator: string, relays: Relay[]): Promise<Report> {
	const { open } = (await import(`/slice-${generator}.js`)) as { open: Open };
	const slice = await open(new URL(`/slice/${generator}.wasm`, location.href));
	const cases: CaseResult[] = [];
	const record = async (name: string, body: () => Promise<string | undefined>) => {
		try {
			cases.push({ name, ok: true, note: await within(body(), 15000, name) });
		} catch (err) {
			cases.push({ name, ok: false, error: describe(err) });
		}
	};
	for (const [name, body] of Object.entries(local)) await record(name, () => body(slice));
	for (const relay of relays)
		for (const [name, body] of Object.entries(remote))
			await record(`${relay.name}: ${name}`, () => body(slice, relay));
	let evaluates = true;
	try {
		new Function("");
	} catch {
		evaluates = false;
	}
	return { generator, eval: evaluates, cases };
}

declare global {
	interface Window {
		sliceCases: typeof run;
	}
}
window.sliceCases = run;
