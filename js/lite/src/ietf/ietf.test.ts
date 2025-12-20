import assert from "node:assert";
import test from "node:test";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import * as GoAway from "./goaway.ts";
import { PublishDone } from "./publish.ts";
import * as Announce from "./publish_namespace.ts";
import * as Subscribe from "./subscribe.ts";
import * as Track from "./track.ts";

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

// Helper to encode a message
async function encodeMessage<T extends { encode(w: Writer): Promise<void> }>(message: T): Promise<Uint8Array> {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream);
	await message.encode(writer);
	writer.close();
	await writer.closed;
	return concatChunks(written);
}

// Helper to decode a message
async function decodeMessage<T>(bytes: Uint8Array, decoder: (r: Reader) => Promise<T>): Promise<T> {
	const reader = new Reader(undefined, bytes);
	return await decoder(reader);
}

// Subscribe tests
test("Subscribe: round trip", async () => {
	const msg = new Subscribe.Subscribe(1n, Path.from("test"), "video", 128);

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, Subscribe.Subscribe.decode);

	assert.strictEqual(decoded.requestId, 1n);
	assert.strictEqual(decoded.trackNamespace, "test");
	assert.strictEqual(decoded.trackName, "video");
	assert.strictEqual(decoded.subscriberPriority, 128);
});

test("Subscribe: nested namespace", async () => {
	const msg = new Subscribe.Subscribe(100n, Path.from("conference/room123"), "audio", 255);

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, Subscribe.Subscribe.decode);

	assert.strictEqual(decoded.trackNamespace, "conference/room123");
});

test("SubscribeOk: without largest", async () => {
	const msg = new Subscribe.SubscribeOk(42n, 43n);

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, Subscribe.SubscribeOk.decode);

	assert.strictEqual(decoded.requestId, 42n);
	assert.strictEqual(decoded.trackAlias, 43n);
});

test("SubscribeError: round trip", async () => {
	const msg = new Subscribe.SubscribeError(123n, 500, "Not found");

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, Subscribe.SubscribeError.decode);

	assert.strictEqual(decoded.requestId, 123n);
	assert.strictEqual(decoded.errorCode, 500);
	assert.strictEqual(decoded.reasonPhrase, "Not found");
});

test("Unsubscribe: round trip", async () => {
	const msg = new Subscribe.Unsubscribe(999n);

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, Subscribe.Unsubscribe.decode);

	assert.strictEqual(decoded.requestId, 999n);
});

test("PublishDone: basic test", async () => {
	const msg = new PublishDone(10n, 0, "complete");

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, PublishDone.decode);

	assert.strictEqual(decoded.requestId, 10n);
	assert.strictEqual(decoded.statusCode, 0);
	assert.strictEqual(decoded.reasonPhrase, "complete");
});

test("PublishDone: with error", async () => {
	const msg = new PublishDone(10n, 1, "error");

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, PublishDone.decode);

	assert.strictEqual(decoded.requestId, 10n);
	assert.strictEqual(decoded.statusCode, 1);
	assert.strictEqual(decoded.reasonPhrase, "error");
});

// Announce/PublishNamespace tests
test("PublishNamespace: round trip", async () => {
	const msg = new Announce.PublishNamespace(1n, Path.from("test/broadcast"));

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, Announce.PublishNamespace.decode);

	assert.strictEqual(decoded.requestId, 1n);
	assert.strictEqual(decoded.trackNamespace, "test/broadcast");
});

test("PublishNamespaceOk: round trip", async () => {
	const msg = new Announce.PublishNamespaceOk(2n);

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, Announce.PublishNamespaceOk.decode);

	assert.strictEqual(decoded.requestId, 2n);
});

test("PublishNamespaceError: round trip", async () => {
	const msg = new Announce.PublishNamespaceError(3n, 404, "Unauthorized");

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, Announce.PublishNamespaceError.decode);

	assert.strictEqual(decoded.requestId, 3n);
	assert.strictEqual(decoded.errorCode, 404);
	assert.strictEqual(decoded.reasonPhrase, "Unauthorized");
});

test("PublishNamespaceDone: round trip", async () => {
	const msg = new Announce.PublishNamespaceDone(Path.from("old/stream"));

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, Announce.PublishNamespaceDone.decode);

	assert.strictEqual(decoded.trackNamespace, "old/stream");
});

test("PublishNamespaceCancel: round trip", async () => {
	const msg = new Announce.PublishNamespaceCancel(Path.from("canceled"), 1, "Shutdown");

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, Announce.PublishNamespaceCancel.decode);

	assert.strictEqual(decoded.trackNamespace, "canceled");
	assert.strictEqual(decoded.errorCode, 1);
	assert.strictEqual(decoded.reasonPhrase, "Shutdown");
});

// GoAway tests
test("GoAway: with URL", async () => {
	const msg = new GoAway.GoAway("https://example.com/new");

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, GoAway.GoAway.decode);

	assert.strictEqual(decoded.newSessionUri, "https://example.com/new");
});

test("GoAway: empty", async () => {
	const msg = new GoAway.GoAway("");

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, GoAway.GoAway.decode);

	assert.strictEqual(decoded.newSessionUri, "");
});

// Track tests
test("TrackStatusRequest: round trip", async () => {
	const msg = new Track.TrackStatusRequest(Path.from("video/stream"), "main");

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, Track.TrackStatusRequest.decode);

	assert.strictEqual(decoded.trackNamespace, "video/stream");
	assert.strictEqual(decoded.trackName, "main");
});

test("TrackStatus: round trip", async () => {
	const msg = new Track.TrackStatus(Path.from("test"), "status", 200, 42n, 100n);

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, Track.TrackStatus.decode);

	assert.strictEqual(decoded.trackNamespace, "test");
	assert.strictEqual(decoded.trackName, "status");
	assert.strictEqual(decoded.statusCode, 200);
	assert.strictEqual(decoded.lastGroupId, 42n);
	assert.strictEqual(decoded.lastObjectId, 100n);
});

// Validation tests
test("Subscribe: rejects invalid filter type", async () => {
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
		await decodeMessage(invalidBytes, Subscribe.Subscribe.decode);
	});
});

test("SubscribeOk: rejects non-zero expires", async () => {
	const invalidBytes = new Uint8Array([
		0x01, // subscribe_id
		0x05, // INVALID: expires = 5
		0x02, // group_order
		0x00, // content_exists
		0x00, // num_params
	]);

	await assert.rejects(async () => {
		await decodeMessage(invalidBytes, Subscribe.SubscribeOk.decode);
	});
});

// Unicode tests
test("SubscribeError: unicode strings", async () => {
	const msg = new Subscribe.SubscribeError(1n, 400, "Error: 错误 🚫");

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, Subscribe.SubscribeError.decode);

	assert.strictEqual(decoded.requestId, 1n);
	assert.strictEqual(decoded.errorCode, 400);
	assert.strictEqual(decoded.reasonPhrase, "Error: 错误 🚫");
});

test("PublishNamespace: unicode namespace", async () => {
	const msg = new Announce.PublishNamespace(1n, Path.from("会议/房间"));

	const encoded = await encodeMessage(msg);
	const decoded = await decodeMessage(encoded, Announce.PublishNamespace.decode);

	assert.strictEqual(decoded.requestId, 1n);
	assert.strictEqual(decoded.trackNamespace, "会议/房间");
});
