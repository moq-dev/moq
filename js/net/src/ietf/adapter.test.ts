import { expect, spyOn, test } from "bun:test";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { ControlStreamAdapter } from "./adapter.ts";
import { toRequestCode } from "./error.ts";
import { GoAway } from "./goaway.ts";
import { PublishNamespace, PublishNamespaceCancel, PublishNamespaceDone } from "./publish_namespace.ts";
import { MaxRequestId, REQUEST_LIMIT, RequestError } from "./request.ts";
import { SubscribeUpdate } from "./subscribe.ts";
import { TrackStatusRequest } from "./track.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

test("draft-14 TRACK_STATUS_OK cannot be routed as NAMESPACE_DONE", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_14);
	const control = await Stream.open(pair.server, { version: Version.DRAFT_14 });
	const adapter = new ControlStreamAdapter(pair.server, control, Version.DRAFT_14, 100n, true);
	const running = adapter.run();
	const peer = await Stream.accept(pair.client, Version.DRAFT_14);
	if (!peer) throw new Error("no control stream");
	await peer.writer.u53(0x0e);
	await peer.writer.u16(0);
	await expect(running).rejects.toThrow("unexpected message 0x0e");
});

// Draft-15 is the interesting one: it names its namespace withdrawals instead of
// numbering them, so the adapter has to resolve them through a map it keeps itself.
const VERSION = Version.DRAFT_15;

/** How long to wait for something the adapter should have done by now. */
const WAIT = 250;

/** Stand up an adapter over a mock transport, plus the peer's view of the control stream. */
async function connect(): Promise<{ adapter: ControlStreamAdapter; peer: Stream }> {
	const pair = createMockTransportPair(ALPN.DRAFT_15);

	const control = await Stream.open(pair.server, { version: VERSION });
	const adapter = new ControlStreamAdapter(pair.server, control, VERSION, 100n, true);
	void adapter.run().catch(() => void 0);

	const peer = await Stream.accept(pair.client, VERSION);
	if (!peer) throw new Error("no control stream");

	return { adapter, peer };
}

/** Announce a namespace from the peer, on its own request. */
async function announce(peer: Stream, requestId: bigint, namespace: Path.Valid): Promise<void> {
	await peer.writer.u53(PublishNamespace.id);
	await new PublishNamespace({ requestId, trackNamespace: namespace }).encode(peer.writer, VERSION);
}

/** Announce a namespace through the adapter to its peer. */
async function announceOutgoing(
	adapter: ControlStreamAdapter,
	requestId: bigint,
	namespace: Path.Valid,
): Promise<Stream> {
	const stream = adapter.openBi();
	await stream.writer.u53(PublishNamespace.id);
	await new PublishNamespace({ requestId, trackNamespace: namespace }).encode(stream.writer, VERSION);
	return stream;
}

/** Withdraw a namespace from the peer, by name as draft-14/15 do. */
async function withdraw(peer: Stream, namespace: Path.Valid): Promise<void> {
	await peer.writer.u53(PublishNamespaceDone.id);
	await new PublishNamespaceDone({ trackNamespace: namespace }).encode(peer.writer, VERSION);
}

/** Reject an outgoing namespace announcement by name. */
async function cancel(peer: Stream, namespace: Path.Valid): Promise<void> {
	await peer.writer.u53(PublishNamespaceCancel.id);
	await new PublishNamespaceCancel({ trackNamespace: namespace, errorCode: 0, reasonPhrase: "" }).encode(
		peer.writer,
		VERSION,
	);
}

/** Accept the virtual stream an announcement opened and consume the announcement itself. */
async function accept(adapter: ControlStreamAdapter): Promise<Stream> {
	const stream = await Promise.race([
		adapter.acceptBi(),
		new Promise<undefined>((resolve) => setTimeout(() => resolve(undefined), WAIT)),
	]);
	if (!stream) throw new Error("no virtual stream");

	expect(await stream.reader.u53()).toBe(PublishNamespace.id);
	await PublishNamespace.decode(stream.reader, VERSION);

	return stream;
}

/** Whether a virtual stream's recv side closed, rather than staying open forever. */
async function closed(stream: Stream): Promise<boolean> {
	return await Promise.race([
		stream.reader.done(),
		new Promise<boolean>((resolve) => setTimeout(() => resolve(false), WAIT)),
	]);
}

/**
 * Draft-14/15 withdrawals name a namespace, so the adapter resolves them through a map it
 * keeps while decoding. A duplicate announcement is refused, but the mapping is written
 * before the subscriber ever sees it: overwriting there would point the first request's DONE
 * at the refused one, which has no stream left, and the announcement would stay up for the
 * rest of the session.
 */
test("a refused duplicate does not strand the first announcement", async () => {
	const { adapter, peer } = await connect();
	const namespace = Path.from("twice");

	await announce(peer, 1n, namespace);
	const first = await accept(adapter);

	// The same namespace again, on its own request.
	await announce(peer, 3n, namespace);
	const second = await accept(adapter);

	// Refused, the way the subscriber refuses a namespace it already has.
	await second.writer.u53(RequestError.id);
	await new RequestError({
		requestId: 3n,
		errorCode: toRequestCode("internal", "publish_namespace", VERSION),
		reasonPhrase: "duplicate namespace",
		retryInterval: 0n,
	}).encode(second.writer, VERSION);
	second.close();

	// The first request is still the one that owns the name, so its DONE withdraws it.
	await withdraw(peer, namespace);
	expect(await closed(first)).toBe(true);
});

/**
 * A withdrawal the adapter cannot resolve is the peer tidying up after a refusal, or a
 * request that is already gone. Throwing there tears down the control stream, which takes
 * every healthy request on the session with it.
 */
test("an unresolvable withdrawal leaves the session open", async () => {
	const { adapter, peer } = await connect();

	await withdraw(peer, Path.from("ghost"));

	// The adapter is still routing: a real announcement arrives after the dropped one.
	await announce(peer, 1n, Path.from("real"));
	const stream = await accept(adapter);

	await withdraw(peer, Path.from("real"));
	expect(await closed(stream)).toBe(true);
});

/**
 * Closing a request has to release both halves of the mapping. A namespace left behind
 * would refuse its own re-announcement for the rest of the session, since the first
 * announcement wins.
 */
test("a cancel releases the namespace for the next announcement", async () => {
	const { adapter, peer } = await connect();
	const namespace = Path.from("recycled");

	const first = await announceOutgoing(adapter, 0n, namespace);
	await cancel(peer, namespace);
	expect(await closed(first)).toBe(true);

	// The name is free again, so a later request can take it and be canceled.
	const second = await announceOutgoing(adapter, 2n, namespace);
	await cancel(peer, namespace);
	expect(await closed(second)).toBe(true);
});

/**
 * A relay may advertise the same namespace in both directions. DONE withdraws the
 * peer's incoming announcement, while CANCEL rejects the local outgoing one.
 */
test("withdrawals distinguish the same namespace by direction", async () => {
	const { adapter, peer } = await connect();
	const namespace = Path.from("mesh");

	const outgoing = await announceOutgoing(adapter, 0n, namespace);
	await announce(peer, 1n, namespace);
	const incoming = await accept(adapter);

	await withdraw(peer, namespace);
	expect(await closed(incoming)).toBe(true);

	await cancel(peer, namespace);
	expect(await closed(outgoing)).toBe(true);
});

/**
 * Draft-14 to -16 carry GOAWAY on the shared control stream. The adapter decodes it and keeps
 * routing, so the session serves its groups in flight while the caller migrates.
 */
test("the control stream adapter decodes a GOAWAY and keeps running", async () => {
	const { adapter, peer } = await connect();

	await peer.writer.u53(GoAway.id);
	await new GoAway({ newSessionUri: "https://relay.example/next" }).encode(peer.writer, VERSION);

	const drain = await adapter.goaway;
	expect(drain.uri).toBe("https://relay.example/next");
	// These drafts carry no timeout: absence means the caller's cap, never a zero handover.
	expect(drain.timeout).toBeUndefined();

	// Still routing: a later announcement opens its virtual stream.
	await announce(peer, 1n, Path.from("still"));
	await accept(adapter);
});

test("a server adapter rejects a client GOAWAY that names a redirect", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_15);
	const control = await Stream.open(pair.server, { version: VERSION });
	const adapter = new ControlStreamAdapter(pair.server, control, VERSION, 100n, false);
	const running = adapter.run();
	const peer = await Stream.accept(pair.client, VERSION);
	if (!peer) throw new Error("no control stream");

	await peer.writer.u53(GoAway.id);
	await new GoAway({ newSessionUri: "https://other.example/" }).encode(peer.writer, VERSION);
	await expect(running).rejects.toThrow("client GOAWAY must not name a redirect");
});

test("a second GOAWAY on the control stream closes the session", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_15);
	const control = await Stream.open(pair.server, { version: VERSION });
	const adapter = new ControlStreamAdapter(pair.server, control, VERSION, 100n, true);
	const running = adapter.run();
	const peer = await Stream.accept(pair.client, VERSION);
	if (!peer) throw new Error("no control stream");

	for (let i = 0; i < 2; i++) {
		await peer.writer.u53(GoAway.id);
		await new GoAway({ newSessionUri: "" }).encode(peer.writer, VERSION);
	}
	await expect(running).rejects.toThrow("duplicate GOAWAY");
});

// Draft-14 to -16 session codes. Past the advertised maximum is TOO_MANY_REQUESTS;
// a parity or duplicate error is INVALID_REQUEST_ID.
const TOO_MANY_REQUESTS = 0x7;
const INVALID_REQUEST_ID = 0x4;

const WINDOW_DRAFTS = [
	[Version.DRAFT_14, ALPN.DRAFT_14],
	[Version.DRAFT_15, ALPN.DRAFT_15],
	[Version.DRAFT_16, ALPN.DRAFT_16],
] as const;

/** Adapter plus the peer's control stream. `client` is this side, so the peer uses the other parity. */
async function windowed(
	version: IetfVersion,
	alpn: string,
	peerLimit: bigint,
	client: boolean,
): Promise<{
	pair: ReturnType<typeof createMockTransportPair>;
	adapter: ControlStreamAdapter;
	peer: Stream;
	running: Promise<void>;
}> {
	const pair = createMockTransportPair(alpn);
	const control = await Stream.open(pair.server, { version });
	const adapter = new ControlStreamAdapter(pair.server, control, version, 100n, client, peerLimit);
	const running = adapter.run();
	const peer = await Stream.accept(pair.client, version);
	if (!peer) throw new Error("no control stream");
	return { pair, adapter, peer, running };
}

/** A new request of the peer's parity. TRACK_STATUS spends an id and holds no namespace. */
async function trackStatus(peer: Stream, version: IetfVersion, requestId: bigint): Promise<void> {
	await peer.writer.u53(TrackStatusRequest.id);
	await new TrackStatusRequest({
		requestId,
		trackNamespace: Path.from("t"),
		trackName: "a",
	}).encode(peer.writer, version);
}

/** Read grant updates until one is at least `target`. Hangs when no grant is sent, which is the bug. */
async function granted(peer: Stream, version: IetfVersion, target: bigint): Promise<bigint> {
	let max = 0n;
	while (max < target) {
		const type = await peer.reader.u53();
		if (type !== MaxRequestId.id) {
			const size = await peer.reader.u16();
			await peer.reader.read(size);
			continue;
		}
		const msg = await MaxRequestId.decode(peer.reader, version);
		if (msg.requestId > max) max = msg.requestId;
	}
	return max;
}

test("an id at the advertised maximum closes the session", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS) {
		for (const client of [false, true]) {
			// Correct parity, one past the window. A wrong-parity id that is also past the
			// window is still too many: the maximum is checked first.
			const past = client ? 5n : 4n;
			const pastWrong = client ? 4n : 5n;
			for (const requestId of [past, pastWrong]) {
				const { pair, peer, running } = await windowed(version, alpn, 4n, client);
				await trackStatus(peer, version, requestId);
				await expect(running).rejects.toThrow("request id exceeds max");
				expect((await pair.client.closed).closeCode).toBe(TOO_MANY_REQUESTS);
			}
		}
	}
});

test("a request id with the wrong parity closes the session", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS) {
		for (const client of [false, true]) {
			const wrong = client ? 0n : 1n;
			const { pair, peer, running } = await windowed(version, alpn, 4n, client);
			await trackStatus(peer, version, wrong);
			await expect(running).rejects.toThrow("wrong parity");
			expect((await pair.client.closed).closeCode).toBe(INVALID_REQUEST_ID);
		}
	}
});

test("a second use of an open request id closes the session", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS) {
		const { pair, peer, running } = await windowed(version, alpn, 4n, false);
		await trackStatus(peer, version, 0n);
		await trackStatus(peer, version, 0n);
		await expect(running).rejects.toThrow("duplicate request id");
		expect((await pair.client.closed).closeCode).toBe(INVALID_REQUEST_ID);
	}
});

test("closing a request grants one more id of the peer's parity", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS) {
		for (const client of [false, true]) {
			const ids = client ? [1n, 3n, 5n] : [0n, 2n, 4n];
			const { adapter, peer, running } = await windowed(version, alpn, 4n, client);
			const failed = running.then(
				() => undefined,
				(err: unknown) => err,
			);
			for (const requestId of ids) {
				await trackStatus(peer, version, requestId);
				const stream = await adapter.acceptBi();
				if (!stream) throw new Error(`no stream for ${requestId}`);
				stream.close();
			}
			// Three closes, each raising the exclusive max by 2.
			expect(await granted(peer, version, 10n)).toBeGreaterThanOrEqual(10n);
			expect(await Promise.race([failed, Promise.resolve(undefined)])).toBeUndefined();
		}
	}
});

test("a request update spends an id and frees it immediately", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => undefined);
	try {
		for (const [version, alpn] of WINDOW_DRAFTS) {
			for (const client of [false, true]) {
				const updateId = client ? 1n : 0n;
				const nextId = client ? 3n : 2n;
				const { adapter, peer, running } = await windowed(version, alpn, 2n, client);
				const failed = running.then(
					() => undefined,
					(err: unknown) => err,
				);
				await peer.writer.u53(SubscribeUpdate.id);
				await new SubscribeUpdate({ requestId: updateId }).encode(peer.writer, version);
				await trackStatus(peer, version, nextId);
				const stream = await adapter.acceptBi();
				if (!stream) throw new Error("update held the only slot");
				stream.close();
				expect(await granted(peer, version, 4n)).toBeGreaterThanOrEqual(4n);
				expect(await Promise.race([failed, Promise.resolve(undefined)])).toBeUndefined();
			}
		}
	} finally {
		warn.mockRestore();
	}
});

test("an update naming an open request does not grow the window", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => undefined);
	try {
		for (const [version, alpn] of WINDOW_DRAFTS) {
			for (const client of [false, true]) {
				const heldId = client ? 1n : 0n;
				const nextId = client ? 3n : 2n;
				const { pair, adapter, peer, running } = await windowed(version, alpn, 2n, client);
				await trackStatus(peer, version, heldId);
				const held = await adapter.acceptBi();
				if (!held) throw new Error("no stream");
				await peer.writer.u53(SubscribeUpdate.id);
				await new SubscribeUpdate({ requestId: heldId }).encode(peer.writer, version);
				await trackStatus(peer, version, nextId);
				await expect(running).rejects.toThrow("request id exceeds max");
				expect((await pair.client.closed).closeCode).toBe(TOO_MANY_REQUESTS);
			}
		}
	} finally {
		warn.mockRestore();
	}
});

/**
 * SETUP advertises 42069 and used to stop there. One more even id than that window
 * (0, 2, ..., 42070) only completes when each close raises the maximum.
 */
test(
	"more requests than the setup window complete on one session",
	async () => {
		const version = Version.DRAFT_15;
		const { pair, adapter, peer, running } = await windowed(version, ALPN.DRAFT_15, REQUEST_LIMIT, false);
		const failed = running.then(
			() => undefined,
			(err: unknown) => err,
		);

		let limit = 0n;
		let wake: (() => void) | undefined;
		const reading = (async () => {
			try {
				for (;;) {
					if (await peer.reader.done()) return;
					const type = await peer.reader.u53();
					if (type !== MaxRequestId.id) {
						const size = await peer.reader.u16();
						await peer.reader.read(size);
						continue;
					}
					const msg = await MaxRequestId.decode(peer.reader, version);
					if (msg.requestId > limit) {
						limit = msg.requestId;
						wake?.();
					}
				}
			} catch {
				// The session closed under the reader.
			}
		})();

		try {
			for (let id = 0n; id <= REQUEST_LIMIT + 1n; id += 2n) {
				await trackStatus(peer, version, id);
				const stream = await adapter.acceptBi();
				if (!stream) throw new Error(`session closed at request ${id}`);
				stream.close();
			}

			if (limit <= REQUEST_LIMIT) {
				await new Promise<void>((resolve, reject) => {
					const timer = setTimeout(
						() => reject(new Error("peer was not granted a larger request window")),
						1000,
					);
					wake = () => {
						if (limit <= REQUEST_LIMIT) return;
						clearTimeout(timer);
						resolve();
					};
					if (limit > REQUEST_LIMIT) wake();
				});
			}
			expect(limit).toBeGreaterThan(REQUEST_LIMIT);
			expect(await Promise.race([failed, Promise.resolve(undefined)])).toBeUndefined();
		} finally {
			pair.server.close();
			await running.catch(() => undefined);
			await reading;
		}
	},
	{ timeout: 60_000 },
);
