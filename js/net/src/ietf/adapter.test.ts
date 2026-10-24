import { expect, spyOn, test } from "bun:test";
import { SessionCode } from "../error.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { ControlStreamAdapter } from "./adapter.ts";
import { toRequestCode } from "./error.ts";
import { GoAway } from "./goaway.ts";
import {
	PublishNamespace,
	PublishNamespaceCancel,
	PublishNamespaceDone,
	PublishNamespaceUpdate,
} from "./publish_namespace.ts";
import { MaxRequestId, RequestError, RequestOk } from "./request.ts";
import { initialMaxRequestId, REQUEST_WINDOW } from "./setup_options.ts";
import { Subscribe, SubscribeError, SubscribeUpdate } from "./subscribe.ts";
import { SubscribeNamespaceLegacy } from "./subscribe_namespace.ts";
import { TrackStatusOk, TrackStatusRequest } from "./track.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

test("draft-14 status answers route by request ID and a refusal leaves the session usable", async () => {
	const version = Version.DRAFT_14;
	const pair = createMockTransportPair(ALPN.DRAFT_14);
	const control = await Stream.open(pair.server, { version });
	const adapter = new ControlStreamAdapter(pair.server, control, version, 100n, true);
	void adapter.run().catch(() => {});
	const peer = await Stream.accept(pair.client, version);
	if (!peer) throw new Error("no control stream");
	try {
		for (const refused of [false, true, false]) {
			const requestId = await adapter.nextRequestId();
			if (requestId === undefined) throw new Error("no request ID");
			const stream = await adapter.openBi();
			await stream.writer.u53(TrackStatusRequest.id);
			await new TrackStatusRequest({ requestId, trackNamespace: Path.from("room"), trackName: "video" }).encode(
				stream.writer,
				version,
			);
			expect(await peer.reader.u53()).toBe(TrackStatusRequest.id);
			expect((await TrackStatusRequest.decode(peer.reader, version)).requestId).toBe(requestId);
			if (refused) {
				await peer.writer.u53(0x0f);
				await new SubscribeError({ requestId, errorCode: 4, reasonPhrase: "missing" }).encode(
					peer.writer,
					version,
				);
				expect(await stream.reader.u53()).toBe(0x0f);
				expect((await SubscribeError.decode(stream.reader, version)).requestId).toBe(requestId);
			} else {
				await peer.writer.u53(TrackStatusOk.id(version));
				await new TrackStatusOk({ requestId, largest: { groupId: 4n, objectId: 1n } }).encode(
					peer.writer,
					version,
				);
				expect(await stream.reader.u53()).toBe(TrackStatusOk.id(version));
				const status = await TrackStatusOk.decode(stream.reader, version);
				expect(status.requestId).toBe(requestId);
				expect(status.largest).toEqual({ groupId: 4n, objectId: 1n });
			}
			stream.close();
		}
	} finally {
		adapter.close();
	}
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
	// A writer may yield a task before the rejection assertion below.
	void running.catch(() => {});
	const peer = await Stream.accept(pair.client, VERSION);
	if (!peer) throw new Error("no control stream");

	await peer.writer.u53(GoAway.id);
	await new GoAway({ newSessionUri: "https://other.example/" }).encode(peer.writer, VERSION);
	await expect(running).rejects.toThrow("client GOAWAY must not name a redirect");
});

test("the control stream adapter refuses draft 17 and later", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_17);
	const control = await Stream.open(pair.server, { version: Version.DRAFT_17 });
	expect(() => new ControlStreamAdapter(pair.server, control, Version.DRAFT_17, 100n, true)).toThrow(
		"drafts 14 to 16",
	);
});

test("a second GOAWAY on the control stream closes the session", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_15);
	const control = await Stream.open(pair.server, { version: VERSION });
	const adapter = new ControlStreamAdapter(pair.server, control, VERSION, 100n, true);
	const running = adapter.run();
	// A writer may yield a task before the rejection assertion below.
	void running.catch(() => {});
	const peer = await Stream.accept(pair.client, VERSION);
	if (!peer) throw new Error("no control stream");

	for (let i = 0; i < 2; i++) {
		await peer.writer.u53(GoAway.id);
		await new GoAway({ newSessionUri: "" }).encode(peer.writer, VERSION);
	}
	await expect(running).rejects.toThrow("duplicate GOAWAY");
});

// Past the advertised maximum, or more open requests than the window, is TOO_MANY_REQUESTS.
// A parity or duplicate error is draft-14 to -16's INVALID_REQUEST_ID.
const INVALID_REQUEST_ID = 0x4;

const WINDOW_DRAFTS = [
	[Version.DRAFT_14, ALPN.DRAFT_14],
	[Version.DRAFT_15, ALPN.DRAFT_15],
	[Version.DRAFT_16, ALPN.DRAFT_16],
] as const;

/**
 * Adapter admitting `window` open requests, plus the peer's control stream.
 * `client` is this side, so the peer uses the other parity.
 */
async function windowed(
	version: IetfVersion,
	alpn: string,
	window: bigint,
	client: boolean,
): Promise<{
	pair: ReturnType<typeof createMockTransportPair>;
	adapter: ControlStreamAdapter;
	peer: Stream;
	running: Promise<void>;
}> {
	const pair = createMockTransportPair(alpn);
	const control = await Stream.open(pair.server, { version });
	const adapter = new ControlStreamAdapter(pair.server, control, version, 100n, client, window);
	const running = adapter.run();
	// A peer write may yield a browser task before the assertion awaits this rejection.
	void running.catch(() => {});
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

/** A SUBSCRIBE as `requestId`, returning its virtual stream past the initial message. */
async function subscribe(adapter: ControlStreamAdapter, peer: Stream, version: IetfVersion, requestId: bigint) {
	await peer.writer.u53(Subscribe.id);
	await new Subscribe({
		requestId,
		trackNamespace: Path.from("update"),
		trackName: "video",
		subscriberPriority: 128,
	}).encode(peer.writer, version);

	const stream = await adapter.acceptBi();
	if (!stream) throw new Error(`no stream for ${requestId}`);
	expect(await stream.reader.u53()).toBe(Subscribe.id);
	await Subscribe.decode(stream.reader, version);
	return stream;
}

/** An update spending `own` that changes `target`. */
async function update(peer: Stream, version: IetfVersion, own: bigint, target: bigint): Promise<void> {
	await peer.writer.u53(SubscribeUpdate.id);
	await new SubscribeUpdate({ requestId: target, ownRequestId: own }).encode(peer.writer, version);
}

/** The request named by the next update delivered to `stream`. */
async function updated(stream: Stream, version: IetfVersion): Promise<bigint> {
	const typeId = await Promise.race([
		stream.reader.u53(),
		new Promise<undefined>((resolve) => setTimeout(() => resolve(undefined), WAIT)),
	]);
	if (typeId === undefined) throw new Error("update did not reach its target");
	expect(typeId).toBe(SubscribeUpdate.id);
	return (await SubscribeUpdate.decode(stream.reader, version)).requestId;
}

/** The next MAX_REQUEST_ID on the peer's control stream, skipping anything else. */
async function nextGrant(peer: Stream, version: IetfVersion): Promise<bigint> {
	for (;;) {
		const type = await peer.reader.u53();
		if (type === MaxRequestId.id) return (await MaxRequestId.decode(peer.reader, version)).requestId;
		const size = await peer.reader.u16();
		await peer.reader.read(size);
	}
}

/** Resolves with the run loop's error, or undefined while it is still running. */
function settled(running: Promise<void>): () => Promise<unknown> {
	const failed = running.then(
		() => undefined,
		(err: unknown) => err,
	);
	return () => Promise.race([failed, Promise.resolve(undefined)]);
}

test("an id at the advertised maximum closes the session", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS) {
		for (const client of [false, true]) {
			// A window of 2 admits ids below 4 from a client peer and below 5 from a server peer.
			// A wrong-parity id that is also past the maximum is still too many: the maximum is checked first.
			const past = client ? 5n : 4n;
			const pastWrong = client ? 6n : 5n;
			for (const requestId of [past, pastWrong]) {
				const { pair, peer, running } = await windowed(version, alpn, 2n, client);
				await trackStatus(peer, version, requestId);
				await expect(running).rejects.toThrow("request id exceeds max");
				expect((await pair.client.closed).closeCode).toBe(SessionCode.TooManyRequests);
			}
		}
	}
});

test("a request id with the wrong parity closes the session", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS) {
		for (const client of [false, true]) {
			const wrong = client ? 0n : 1n;
			const { pair, peer, running } = await windowed(version, alpn, 2n, client);
			await trackStatus(peer, version, wrong);
			await expect(running).rejects.toThrow("wrong parity");
			expect((await pair.client.closed).closeCode).toBe(INVALID_REQUEST_ID);
		}
	}
});

test("a second use of an open request id closes the session", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS) {
		const { pair, peer, running } = await windowed(version, alpn, 1n, false);
		await trackStatus(peer, version, 0n);
		await trackStatus(peer, version, 0n);
		await expect(running).rejects.toThrow("request id was already spent or out of sequence");
		expect((await pair.client.closed).closeCode).toBe(INVALID_REQUEST_ID);
	}
});

test("once half the window closes, one MAX_REQUEST_ID grants the ids it spent", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS) {
		for (const client of [false, true]) {
			const ids = client ? [1n, 3n] : [0n, 2n];
			const { adapter, peer, running } = await windowed(version, alpn, 4n, client);
			const state = settled(running);
			for (const requestId of ids) {
				await trackStatus(peer, version, requestId);
				const stream = await adapter.acceptBi();
				if (!stream) throw new Error(`no stream for ${requestId}`);
				stream.close();
			}
			// One close is below half the window, so the first grant covers both.
			expect(await nextGrant(peer, version)).toBe(initialMaxRequestId(!client, 4n) + 4n);
			expect(await state()).toBeUndefined();
		}
	}
});

test("a request the peer closes is granted back", async () => {
	const { adapter, peer, running } = await windowed(VERSION, ALPN.DRAFT_15, 1n, true);
	const state = settled(running);
	await announce(peer, 1n, Path.from("room"));
	const stream = await adapter.acceptBi();
	if (!stream) throw new Error("no stream");
	await withdraw(peer, Path.from("room"));
	expect(await nextGrant(peer, VERSION)).toBe(initialMaxRequestId(false, 1n) + 2n);
	expect(await state()).toBeUndefined();
});

// A single-id update cannot tell its own Request ID from the target, so these carry both.
test("drafts 14 to 16 route an update to its second request id", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS) {
		const { adapter, peer, running } = await windowed(version, alpn, 4n, false);
		const state = settled(running);
		const target = await subscribe(adapter, peer, version, 0n);
		await update(peer, version, 2n, 0n);
		expect(await updated(target, version)).toBe(0n);
		expect(await state()).toBeUndefined();
	}
});

// The update's own id is retired, not the target's, so half a window of updates earns a grant.
test("updates to an open request earn a grant and still reach it", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS) {
		const { adapter, peer, running } = await windowed(version, alpn, 4n, false);
		const state = settled(running);
		const target = await subscribe(adapter, peer, version, 0n);
		for (const own of [2n, 4n]) {
			await update(peer, version, own, 0n);
			expect(await updated(target, version)).toBe(0n);
		}
		expect(await nextGrant(peer, version)).toBe(initialMaxRequestId(true, 4n) + 4n);
		expect(await state()).toBeUndefined();
	}
});

test("a request update spends an id and retires it immediately", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => undefined);
	try {
		for (const [version, alpn] of WINDOW_DRAFTS) {
			for (const client of [false, true]) {
				const updateId = client ? 1n : 0n;
				const nextId = client ? 3n : 2n;
				const { adapter, peer, running } = await windowed(version, alpn, 1n, client);
				const state = settled(running);
				// The target is gone, which drops the update but still spends its id.
				await update(peer, version, updateId, 100n);
				// The update's own id is granted back, so the next id is admitted.
				await trackStatus(peer, version, nextId);
				const stream = await adapter.acceptBi();
				if (!stream) throw new Error("update held the only slot");
				expect(await nextGrant(peer, version)).toBe(initialMaxRequestId(!client, 1n) + 2n);
				expect(await state()).toBeUndefined();
			}
		}
	} finally {
		warn.mockRestore();
	}
});

test("an update spending an open request id closes the session", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS) {
		for (const client of [false, true]) {
			const heldId = client ? 1n : 0n;
			const { pair, adapter, peer, running } = await windowed(version, alpn, 2n, client);
			await trackStatus(peer, version, heldId);
			if (!(await adapter.acceptBi())) throw new Error("no stream");
			await update(peer, version, heldId, heldId);
			await expect(running).rejects.toThrow("request id was already spent or out of sequence");
			expect((await pair.client.closed).closeCode).toBe(INVALID_REQUEST_ID);
		}
	}
});

/** Request IDs stay spent after their stream or update retires. */
test("retired and lower request ids close the session", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS) {
		for (const client of [false, true]) {
			const parity = client ? 1n : 0n;
			for (const [first, later] of [
				[parity, parity],
				[parity + 4n, parity + 2n],
			] as const) {
				const { pair, adapter, peer, running } = await windowed(version, alpn, 8n, client);
				for (let id = parity; id <= first; id += 2n) {
					await trackStatus(peer, version, id);
					const stream = await adapter.acceptBi();
					if (!stream) throw new Error("no stream");
					stream.close();
				}
				await trackStatus(peer, version, later);
				await expect(running).rejects.toThrow("request id was already spent or out of sequence");
				expect((await pair.client.closed).closeCode).toBe(INVALID_REQUEST_ID);
			}
		}
	}
});

test("an update id stays spent after the update retires", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS) {
		const { pair, adapter, peer, running } = await windowed(version, alpn, 4n, false);
		await trackStatus(peer, version, 0n);
		if (!(await adapter.acceptBi())) throw new Error("no stream");
		await update(peer, version, 2n, 0n);
		await update(peer, version, 2n, 0n);
		await expect(running).rejects.toThrow("request id was already spent or out of sequence");
		expect((await pair.client.closed).closeCode).toBe(INVALID_REQUEST_ID);
	}
});

/** Three windows of requests, one at a time, only complete when grants keep raising the maximum. */
test("more requests than the setup window complete on one session", async () => {
	const version = Version.DRAFT_15;
	const window = 64n;
	const initial = initialMaxRequestId(true, window);
	const { pair, adapter, peer, running } = await windowed(version, ALPN.DRAFT_15, window, false);
	const state = settled(running);

	let limit = initial;
	const reading = (async () => {
		try {
			for (;;) limit = await nextGrant(peer, version);
		} catch {
			// The session closed under the reader.
		}
	})();

	try {
		for (let id = 0n; id < initial * 3n; id += 2n) {
			await trackStatus(peer, version, id);
			const stream = await adapter.acceptBi();
			if (!stream) throw new Error(`session closed at request ${id}`);
			stream.close();
		}
		expect(await state()).toBeUndefined();
		while (limit < initial * 3n) await new Promise((resolve) => setTimeout(resolve, 0));
		expect(limit).toBeGreaterThanOrEqual(initial * 3n);
	} finally {
		pair.server.close();
		await running.catch(() => undefined);
		await reading;
	}
});

// Both requests and updates allocate IDs, even when an update's target is gone.
test("skipped request and update ids close the session", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS.filter(([version]) => version !== Version.DRAFT_16)) {
		for (const client of [false, true]) {
			for (const prefix of [0n, 1n]) {
				for (const isUpdate of [false, true]) {
					const { pair, peer, running } = await windowed(version, alpn, 8n, client);
					const first = client ? 1n : 0n;
					if (prefix) await trackStatus(peer, version, first);
					const skipped = first + 2n * prefix + 2n;
					if (isUpdate) await update(peer, version, skipped, 100n);
					else await trackStatus(peer, version, skipped);
					await expect(running).rejects.toThrow("request id was already spent or out of sequence");
					expect((await pair.client.closed).closeCode).toBe(SessionCode.InvalidRequestId);
				}
			}
		}
	}
});

// SUBSCRIBE_NAMESPACE uses an independent bidi stream in draft 16, sharing the ID space.
test("draft 16 accepts reordered native and control IDs but never reuses them", async () => {
	for (const client of [false, true]) {
		for (const nativeFirst of [false, true]) {
			const version = Version.DRAFT_16;
			const { pair, adapter, peer, running } = await windowed(version, ALPN.DRAFT_16, 8n, client);
			void running.catch(() => undefined);
			const first = client ? 1n : 0n;
			const native = nativeFirst ? first + 2n : first;
			const control = nativeFirst ? first : first + 2n;
			try {
				if (nativeFirst) adapter.admitRequest(native);
				await trackStatus(peer, version, control);
				const stream = await adapter.acceptBi();
				if (!stream) throw new Error("no reordered control request");
				if (!nativeFirst) adapter.admitRequest(native);
				stream.close();
				adapter.releaseRequest(native);
				await trackStatus(peer, version, first);
				await expect(running).rejects.toThrow("request id was already spent");
				expect((await pair.client.closed).closeCode).toBe(SessionCode.InvalidRequestId);
			} finally {
				peer.close();
				adapter.close();
			}
		}
	}
});

// Draft 16 remembers a skipped stretch as one run, so a jump costs nothing per skipped ID.
test("draft 16 tracks a jump across a production window as one run", async () => {
	const window = REQUEST_WINDOW;
	const { adapter, peer } = await windowed(Version.DRAFT_16, ALPN.DRAFT_16, window, false);
	const top = 2n * (window - 1n);
	try {
		for (const id of [top, 100n, 0n, top - 2n, 2n, 98n]) adapter.admitRequest(id);
		for (const id of [0n, 2n, 98n, 100n, top - 2n, top]) {
			expect(() => adapter.admitRequest(id)).toThrow("request id was already spent");
		}
	} finally {
		peer.close();
		adapter.close();
	}
});

// Splitting runs in descending order spans many chunks; every ID is admitted exactly once.
test("draft 16 admits a scattered window once across run chunks", async () => {
	const window = 4096n;
	const { adapter, peer } = await windowed(Version.DRAFT_16, ALPN.DRAFT_16, window, false);
	const top = 2n * (window - 1n);
	const below = Array.from({ length: Number(window - 1n) }, (_, i) => 2n * (window - 2n - BigInt(i)));
	const ids = [top, ...below.filter((id) => (id / 2n) % 2n === 1n), ...below.filter((id) => (id / 2n) % 2n === 0n)];
	try {
		for (const id of ids) adapter.admitRequest(id);
		for (const id of [0n, 2n, 1024n, top - 2n, top]) {
			expect(() => adapter.admitRequest(id)).toThrow("request id was already spent");
		}
	} finally {
		peer.close();
		adapter.close();
	}
});

// Allocation precedes an asynchronous encode, so a shorter later request can finish first.
test("drafts 14 and 15 send concurrent requests in request id order", async () => {
	for (const [version, alpn] of [
		[Version.DRAFT_14, ALPN.DRAFT_14],
		[Version.DRAFT_15, ALPN.DRAFT_15],
	] as const) {
		const pair = createMockTransportPair(alpn);
		const control = await Stream.open(pair.server, { version });
		const sender = new ControlStreamAdapter(pair.server, control, version, 100n, true);
		void sender.run().catch(() => undefined);
		const peer = await Stream.accept(pair.client, version);
		if (!peer) throw new Error("no control stream");
		const receiver = new ControlStreamAdapter(pair.client, peer, version, 100n, false, 8n);
		void receiver.run().catch(() => undefined);
		try {
			const request = async (namespace: Path.Valid) => {
				const requestId = await sender.nextRequestId();
				if (requestId === undefined) throw new Error("no request id");
				const stream = sender.openBi();
				await stream.writer.u53(SubscribeNamespaceLegacy.id);
				await new SubscribeNamespaceLegacy({ namespace, requestId }).encode(stream.writer, version);
			};
			await Promise.all([request(Path.from("a/b/c/d/e/f/g/h")), request(Path.from("a"))]);

			const received: bigint[] = [];
			for (let i = 0; i < 2; i++) {
				const stream = await receiver.acceptBi();
				if (!stream) throw new Error("the receiver refused a request");
				expect(await stream.reader.u53()).toBe(SubscribeNamespaceLegacy.id);
				received.push((await SubscribeNamespaceLegacy.decode(stream.reader, version)).requestId);
			}
			expect(received).toEqual([0n, 2n]);
		} finally {
			sender.close();
			receiver.close();
		}
	}
});

// Canceling the wait must remove the allocation itself, not just stop awaiting it.
test("a canceled request ID wait never spends a later grant", async () => {
	for (const [version, alpn] of WINDOW_DRAFTS) {
		const pair = createMockTransportPair(alpn);
		const control = await Stream.open(pair.server, { version });
		const adapter = new ControlStreamAdapter(pair.server, control, version, 0n, true);
		const running = adapter.run().catch(() => undefined);
		const peer = await Stream.accept(pair.client, version);
		if (!peer) throw new Error("no control stream");
		try {
			const signal = new AbortController();
			const canceled = adapter.nextRequestId(signal.signal);
			signal.abort(new Error("retired"));
			await expect(canceled).rejects.toThrow("retired");
			await peer.writer.u53(MaxRequestId.id);
			await new MaxRequestId({ requestId: 4n }).encode(peer.writer, version);
			expect(await adapter.nextRequestId()).toBe(0n);
			await expect(adapter.nextRequestId(signal.signal)).rejects.toThrow("retired");
			expect(await adapter.nextRequestId()).toBe(2n);
		} finally {
			peer.close();
			adapter.close();
			void running;
		}
	}
});

// Draft 16 answers our REQUEST_UPDATE by the update's own request ID. The answer reaches the
// request it updated, and a refusal leaves that request open. Draft 15's reaches nothing.
test("an answer to our update reaches the request it updated on draft 16 only", async () => {
	for (const [version, alpn] of [
		[Version.DRAFT_15, ALPN.DRAFT_15],
		[Version.DRAFT_16, ALPN.DRAFT_16],
	] as const) {
		const { adapter, peer, running } = await windowed(version, alpn, 8n, true);
		try {
			const stream = adapter.openBi();
			await stream.writer.u53(PublishNamespace.id);
			await new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("ns") }).encode(
				stream.writer,
				version,
			);
			// The mock transport schedules nothing on timers, so settled promise chains are final.
			const next = async () => {
				let id: number | undefined;
				const read = stream.reader.u53().then((value) => {
					id = value;
				});
				for (let i = 0; i < 20 && id === undefined; i++) await new Promise((resolve) => setImmediate(resolve));
				void read.catch(() => undefined);
				return id;
			};
			const renew = async (own: bigint) => {
				await stream.writer.u53(SubscribeUpdate.id);
				const update =
					version === Version.DRAFT_16
						? new PublishNamespaceUpdate({ requestId: own, namespaceRequestId: 0n, update: {} })
						: new SubscribeUpdate({ requestId: 0n, ownRequestId: own });
				await update.encode(stream.writer, version);
			};
			const ok = async (requestId: bigint) => {
				await peer.writer.u53(RequestOk.id);
				await new RequestOk({ requestId }).encode(peer.writer, version);
			};

			await ok(0n);
			expect(await next()).toBe(RequestOk.id);
			await RequestOk.decode(stream.reader, version);

			await renew(2n);
			await ok(2n);
			await renew(4n);
			await peer.writer.u53(RequestError.id);
			await new RequestError({ requestId: 4n, errorCode: 0, reasonPhrase: "refused" }).encode(
				peer.writer,
				version,
			);
			if (version === Version.DRAFT_15) {
				expect(await next()).toBeUndefined();
				continue;
			}
			expect(await next()).toBe(RequestOk.id);
			await RequestOk.decode(stream.reader, version);
			expect(await next()).toBe(RequestError.id);
			await RequestError.decode(stream.reader, version);

			// Still open: a later message for the request reaches it after the refusal.
			await ok(0n);
			expect(await next()).toBe(RequestOk.id);
		} finally {
			peer.close();
			adapter.close();
			void running;
		}
	}
});
