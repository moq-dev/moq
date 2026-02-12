import assert from "node:assert";
import test from "node:test";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import * as GoAway from "./goaway.ts";
import { Publish, PublishDone } from "./publish.ts";
import * as Announce from "./publish_namespace.ts";
import { RequestError, RequestOk } from "./request.ts";
import * as Setup from "./setup.ts";
import * as Subscribe from "./subscribe.ts";
import * as Track from "./track.ts";
import { type IetfVersion, Version } from "./version.ts";

// Helper to create a writable stream that captures written data
function createTestWritableStream(): { stream: WritableStream<Uint8Array>; written: Uint8Array[] } {
	const written: Uint8Array[] = [];
	const stream = new WritableStream<Uint8Array>({
		write(chunk) {
			written.push(new Uint8Array(chunk));
		},
	});
	return { stream, written };
}

// Helper to concatenate written chunks
function concatChunks(chunks: Uint8Array[]): Uint8Array {
	const totalLength = chunks.reduce((sum, chunk) => sum + chunk.byteLength, 0);
	const result = new Uint8Array(totalLength);
	let offset = 0;
	for (const chunk of chunks) {
		result.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return result;
}

// Helper to encode a versioned message
async function encodeVersioned<T extends { encode(w: Writer, v: IetfVersion): Promise<void> }>(
	message: T,
	version: IetfVersion,
): Promise<Uint8Array> {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream);
	await message.encode(writer, version);
	writer.close();
	await writer.closed;
	return concatChunks(written);
}

// Helper to decode a versioned message
async function decodeVersioned<T>(
	bytes: Uint8Array,
	decoder: (r: Reader, v: IetfVersion) => Promise<T>,
	version: IetfVersion,
): Promise<T> {
	const reader = new Reader(undefined, bytes);
	return await decoder(reader, version);
}

// Subscribe tests (v14)
test("Subscribe v14: round trip", async () => {
	const msg = new Subscribe.Subscribe({
		requestId: 1n,
		trackNamespace: Path.from("test"),
		trackName: "video",
		subscriberPriority: 128,
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Subscribe.Subscribe.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.requestId, 1n);
	assert.strictEqual(decoded.trackNamespace, "test");
	assert.strictEqual(decoded.trackName, "video");
	assert.strictEqual(decoded.subscriberPriority, 128);
});

test("Subscribe v14: nested namespace", async () => {
	const msg = new Subscribe.Subscribe({
		requestId: 100n,
		trackNamespace: Path.from("conference/room123"),
		trackName: "audio",
		subscriberPriority: 255,
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Subscribe.Subscribe.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.trackNamespace, "conference/room123");
});

test("SubscribeOk v14: without largest", async () => {
	const msg = new Subscribe.SubscribeOk({ requestId: 42n, trackAlias: 43n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Subscribe.SubscribeOk.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.requestId, 42n);
	assert.strictEqual(decoded.trackAlias, 43n);
});

// Subscribe tests (v15)
test("Subscribe v15: round trip", async () => {
	const msg = new Subscribe.Subscribe({
		requestId: 1n,
		trackNamespace: Path.from("test"),
		trackName: "video",
		subscriberPriority: 128,
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_15);
	const decoded = await decodeVersioned(encoded, Subscribe.Subscribe.decode, Version.DRAFT_15);

	assert.strictEqual(decoded.requestId, 1n);
	assert.strictEqual(decoded.trackNamespace, "test");
	assert.strictEqual(decoded.trackName, "video");
	assert.strictEqual(decoded.subscriberPriority, 128);
});

test("SubscribeOk v15: round trip", async () => {
	const msg = new Subscribe.SubscribeOk({ requestId: 42n, trackAlias: 43n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_15);
	const decoded = await decodeVersioned(encoded, Subscribe.SubscribeOk.decode, Version.DRAFT_15);

	assert.strictEqual(decoded.requestId, 42n);
	assert.strictEqual(decoded.trackAlias, 43n);
});

test("SubscribeError: round trip", async () => {
	const msg = new Subscribe.SubscribeError({ requestId: 123n, errorCode: 500, reasonPhrase: "Not found" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Subscribe.SubscribeError.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.requestId, 123n);
	assert.strictEqual(decoded.errorCode, 500);
	assert.strictEqual(decoded.reasonPhrase, "Not found");
});

test("Unsubscribe: round trip", async () => {
	const msg = new Subscribe.Unsubscribe({ requestId: 999n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Subscribe.Unsubscribe.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.requestId, 999n);
});

test("PublishDone: basic test", async () => {
	const msg = new PublishDone({ requestId: 10n, statusCode: 0, reasonPhrase: "complete" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, PublishDone.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.requestId, 10n);
	assert.strictEqual(decoded.statusCode, 0);
	assert.strictEqual(decoded.reasonPhrase, "complete");
});

test("PublishDone: with error", async () => {
	const msg = new PublishDone({ requestId: 10n, statusCode: 1, reasonPhrase: "error" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, PublishDone.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.requestId, 10n);
	assert.strictEqual(decoded.statusCode, 1);
	assert.strictEqual(decoded.reasonPhrase, "error");
});

// Announce/PublishNamespace tests
test("PublishNamespace: round trip", async () => {
	const msg = new Announce.PublishNamespace({ requestId: 1n, trackNamespace: Path.from("test/broadcast") });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespace.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.requestId, 1n);
	assert.strictEqual(decoded.trackNamespace, "test/broadcast");
});

test("PublishNamespaceOk: round trip", async () => {
	const msg = new Announce.PublishNamespaceOk({ requestId: 2n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespaceOk.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.requestId, 2n);
});

test("PublishNamespaceError: round trip", async () => {
	const msg = new Announce.PublishNamespaceError({ requestId: 3n, errorCode: 404, reasonPhrase: "Unauthorized" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespaceError.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.requestId, 3n);
	assert.strictEqual(decoded.errorCode, 404);
	assert.strictEqual(decoded.reasonPhrase, "Unauthorized");
});

test("PublishNamespaceDone: round trip", async () => {
	const msg = new Announce.PublishNamespaceDone({ trackNamespace: Path.from("old/stream") });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespaceDone.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.trackNamespace, "old/stream");
});

test("PublishNamespaceCancel: round trip", async () => {
	const msg = new Announce.PublishNamespaceCancel({
		trackNamespace: Path.from("canceled"),
		errorCode: 1,
		reasonPhrase: "Shutdown",
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespaceCancel.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.trackNamespace, "canceled");
	assert.strictEqual(decoded.errorCode, 1);
	assert.strictEqual(decoded.reasonPhrase, "Shutdown");
});

// GoAway tests
test("GoAway: with URL", async () => {
	const msg = new GoAway.GoAway({ newSessionUri: "https://example.com/new" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, GoAway.GoAway.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.newSessionUri, "https://example.com/new");
});

test("GoAway: empty", async () => {
	const msg = new GoAway.GoAway({ newSessionUri: "" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, GoAway.GoAway.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.newSessionUri, "");
});

// Track tests
test("TrackStatusRequest: round trip", async () => {
	const msg = new Track.TrackStatusRequest({ trackNamespace: Path.from("video/stream"), trackName: "main" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Track.TrackStatusRequest.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.trackNamespace, "video/stream");
	assert.strictEqual(decoded.trackName, "main");
});

test("TrackStatus: round trip", async () => {
	const msg = new Track.TrackStatus({
		trackNamespace: Path.from("test"),
		trackName: "status",
		statusCode: 200,
		lastGroupId: 42n,
		lastObjectId: 100n,
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Track.TrackStatus.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.trackNamespace, "test");
	assert.strictEqual(decoded.trackName, "status");
	assert.strictEqual(decoded.statusCode, 200);
	assert.strictEqual(decoded.lastGroupId, 42n);
	assert.strictEqual(decoded.lastObjectId, 100n);
});

// Validation tests
test("Subscribe v14: rejects invalid filter type", async () => {
	const invalidBytes = new Uint8Array([
		0x01, // subscribe_id
		0x02, // track_alias
		0x01, // namespace length
		0x04,
		0x74,
		0x65,
		0x73,
		0x74, // "test"
		0x05,
		0x76,
		0x69,
		0x64,
		0x65,
		0x6f, // "video"
		0x80, // subscriber_priority
		0x02, // group_order
		0x99, // INVALID filter_type
		0x00, // num_params
	]);

	await assert.rejects(async () => {
		await decodeVersioned(invalidBytes, Subscribe.Subscribe.decode, Version.DRAFT_14);
	});
});

test("SubscribeOk v14: rejects non-zero expires", async () => {
	const invalidBytes = new Uint8Array([
		0x01, // subscribe_id
		0x05, // INVALID: expires = 5
		0x02, // group_order
		0x00, // content_exists
		0x00, // num_params
	]);

	await assert.rejects(async () => {
		await decodeVersioned(invalidBytes, Subscribe.SubscribeOk.decode, Version.DRAFT_14);
	});
});

// Unicode tests
test("SubscribeError: unicode strings", async () => {
	const msg = new Subscribe.SubscribeError({ requestId: 1n, errorCode: 400, reasonPhrase: "Error: 错误 🚫" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Subscribe.SubscribeError.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.requestId, 1n);
	assert.strictEqual(decoded.errorCode, 400);
	assert.strictEqual(decoded.reasonPhrase, "Error: 错误 🚫");
});

test("PublishNamespace: unicode namespace", async () => {
	const msg = new Announce.PublishNamespace({ requestId: 1n, trackNamespace: Path.from("会议/房间") });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespace.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.requestId, 1n);
	assert.strictEqual(decoded.trackNamespace, "会议/房间");
});

// Publish v15 tests
test("Publish v15: round trip", async () => {
	const msg = new Publish({
		requestId: 1n,
		trackNamespace: Path.from("test/ns"),
		trackName: "video",
		trackAlias: 42n,
		groupOrder: 0x02,
		contentExists: false,
		largest: undefined,
		forward: true,
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_15);
	const decoded = await decodeVersioned(encoded, Publish.decode, Version.DRAFT_15);

	assert.strictEqual(decoded.requestId, 1n);
	assert.strictEqual(decoded.trackNamespace, "test/ns");
	assert.strictEqual(decoded.trackName, "video");
	assert.strictEqual(decoded.trackAlias, 42n);
	assert.strictEqual(decoded.forward, true);
});

test("Publish v14: round trip", async () => {
	const msg = new Publish({
		requestId: 1n,
		trackNamespace: Path.from("test/ns"),
		trackName: "video",
		trackAlias: 42n,
		groupOrder: 0x02,
		contentExists: true,
		largest: { groupId: 10n, objectId: 5n },
		forward: true,
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Publish.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.requestId, 1n);
	assert.strictEqual(decoded.trackNamespace, "test/ns");
	assert.strictEqual(decoded.trackName, "video");
	assert.strictEqual(decoded.trackAlias, 42n);
	assert.strictEqual(decoded.forward, true);
	assert.strictEqual(decoded.contentExists, true);
	assert.strictEqual(decoded.largest?.groupId, 10n);
	assert.strictEqual(decoded.largest?.objectId, 5n);
});

// ClientSetup v15 tests
test("ClientSetup v15: round trip", async () => {
	const msg = new Setup.ClientSetup({ versions: [Version.DRAFT_15] });

	const encoded = await encodeVersioned(msg, Version.DRAFT_15);
	const decoded = await decodeVersioned(encoded, Setup.ClientSetup.decode, Version.DRAFT_15);

	assert.strictEqual(decoded.versions.length, 1);
	assert.strictEqual(decoded.versions[0], Version.DRAFT_15);
});

test("ClientSetup v14: round trip", async () => {
	const msg = new Setup.ClientSetup({ versions: [Version.DRAFT_14] });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Setup.ClientSetup.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.versions.length, 1);
	assert.strictEqual(decoded.versions[0], Version.DRAFT_14);
});

// ServerSetup v15 tests
test("ServerSetup v15: round trip", async () => {
	const msg = new Setup.ServerSetup({ version: Version.DRAFT_15 });

	const encoded = await encodeVersioned(msg, Version.DRAFT_15);
	const decoded = await decodeVersioned(encoded, Setup.ServerSetup.decode, Version.DRAFT_15);

	assert.strictEqual(decoded.version, Version.DRAFT_15);
});

test("ServerSetup v14: round trip", async () => {
	const msg = new Setup.ServerSetup({ version: Version.DRAFT_14 });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Setup.ServerSetup.decode, Version.DRAFT_14);

	assert.strictEqual(decoded.version, Version.DRAFT_14);
});

// RequestOk / RequestError v15 tests
test("RequestOk: round trip", async () => {
	const msg = new RequestOk({ requestId: 42n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_15);
	const decoded = await decodeVersioned(encoded, RequestOk.decode, Version.DRAFT_15);

	assert.strictEqual(decoded.requestId, 42n);
});

test("RequestError v15: round trip", async () => {
	const msg = new RequestError({ requestId: 99n, errorCode: 500, reasonPhrase: "Internal error" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_15);
	const decoded = await decodeVersioned(encoded, RequestError.decode, Version.DRAFT_15);

	assert.strictEqual(decoded.requestId, 99n);
	assert.strictEqual(decoded.errorCode, 500);
	assert.strictEqual(decoded.reasonPhrase, "Internal error");
	assert.strictEqual(decoded.retryInterval, 0n);
});

test("RequestError v16: round trip with retryInterval", async () => {
	const msg = new RequestError({
		requestId: 99n,
		errorCode: 500,
		reasonPhrase: "Internal error",
		retryInterval: 5000n,
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_16);
	const decoded = await decodeVersioned(encoded, RequestError.decode, Version.DRAFT_16);

	assert.strictEqual(decoded.requestId, 99n);
	assert.strictEqual(decoded.errorCode, 500);
	assert.strictEqual(decoded.reasonPhrase, "Internal error");
	assert.strictEqual(decoded.retryInterval, 5000n);
});
