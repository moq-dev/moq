import { expect, test } from "bun:test";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { ControlStreamAdapter } from "./adapter.ts";
import { toRequestCode } from "./error.ts";
import { GoAway } from "./goaway.ts";
import { PublishNamespace, PublishNamespaceCancel, PublishNamespaceDone } from "./publish_namespace.ts";
import { RequestError } from "./request.ts";
import { Subscribe, SubscribeUpdate } from "./subscribe.ts";
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

function alpnFor(version: IetfVersion): string {
	switch (version) {
		case Version.DRAFT_14:
			return ALPN.DRAFT_14;
		case Version.DRAFT_15:
			return ALPN.DRAFT_15;
		case Version.DRAFT_16:
			return ALPN.DRAFT_16;
		case Version.DRAFT_17:
			return ALPN.DRAFT_17;
		case Version.DRAFT_18:
			return ALPN.DRAFT_18;
		default:
			throw new Error(`no adapter routing test for ${version}`);
	}
}

/**
 * Open a subscription on `target`, then deliver an update. Returns the request
 * id the adapter delivered it to.
 */
async function routedUpdate(version: IetfVersion, target: bigint, own: bigint | undefined): Promise<bigint> {
	const pair = createMockTransportPair(alpnFor(version));
	const control = await Stream.open(pair.server, { version });
	const adapter = new ControlStreamAdapter(pair.server, control, version, 100n, true);
	void adapter.run().catch(() => void 0);

	const peer = await Stream.accept(pair.client, version);
	if (!peer) throw new Error("no control stream");

	await peer.writer.u53(Subscribe.id);
	await new Subscribe({
		requestId: target,
		trackNamespace: Path.from("update"),
		trackName: "video",
		subscriberPriority: 128,
	}).encode(peer.writer, version);

	const stream = await Promise.race([
		adapter.acceptBi(),
		new Promise<undefined>((resolve) => setTimeout(() => resolve(undefined), WAIT)),
	]);
	if (!stream) throw new Error("no virtual stream");
	expect(await stream.reader.u53()).toBe(Subscribe.id);
	await Subscribe.decode(stream.reader, version);

	await peer.writer.u53(SubscribeUpdate.id);
	await new SubscribeUpdate({ requestId: target, ownRequestId: own }).encode(peer.writer, version);

	const typeId = await Promise.race([
		stream.reader.u53(),
		new Promise<undefined>((resolve) => setTimeout(() => resolve(undefined), WAIT)),
	]);
	if (typeId === undefined) throw new Error(`${version}: update ${own} did not reach ${target}`);
	expect(typeId).toBe(SubscribeUpdate.id);
	const update = await SubscribeUpdate.decode(stream.reader, version);
	return update.requestId;
}

test("drafts 14 to 16 route an update to its second request id", async () => {
	const own = 10n;
	const target = 4n;
	for (const version of [Version.DRAFT_14, Version.DRAFT_15, Version.DRAFT_16] as const) {
		expect(await routedUpdate(version, target, own)).toBe(target);
	}
});

test("draft 17 and later route an update by its request id", async () => {
	const target = 10n;
	for (const version of [Version.DRAFT_17, Version.DRAFT_18] as const) {
		expect(await routedUpdate(version, target, undefined)).toBe(target);
	}
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
