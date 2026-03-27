import { expect, test } from "bun:test";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import * as Varint from "../varint.ts";
import * as GoAway from "./goaway.ts";
import { SetupOptions } from "./parameters.ts";
import { Publish, PublishDone } from "./publish.ts";
import * as Announce from "./publish_namespace.ts";
import { RequestError, RequestOk } from "./request.ts";
import * as Setup from "./setup.ts";
import * as Subscribe from "./subscribe.ts";
import * as SubscribeNamespace from "./subscribe_namespace.ts";
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
	const writer = new Writer(stream, version);
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
	const reader = new Reader(undefined, bytes, version);
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

	expect(decoded.requestId).toBe(1n);
	expect(decoded.trackNamespace).toBe("test" as Path.Valid);
	expect(decoded.trackName).toBe("video");
	expect(decoded.subscriberPriority).toBe(128);
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

	expect(decoded.trackNamespace).toBe("conference/room123" as Path.Valid);
});

test("SubscribeOk v14: without largest", async () => {
	const msg = new Subscribe.SubscribeOk({ requestId: 42n, trackAlias: 43n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Subscribe.SubscribeOk.decode, Version.DRAFT_14);

	expect(decoded.requestId).toBe(42n);
	expect(decoded.trackAlias).toBe(43n);
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

	expect(decoded.requestId).toBe(1n);
	expect(decoded.trackNamespace).toBe("test" as Path.Valid);
	expect(decoded.trackName).toBe("video");
	expect(decoded.subscriberPriority).toBe(128);
});

test("SubscribeOk v15: round trip", async () => {
	const msg = new Subscribe.SubscribeOk({ requestId: 42n, trackAlias: 43n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_15);
	const decoded = await decodeVersioned(encoded, Subscribe.SubscribeOk.decode, Version.DRAFT_15);

	expect(decoded.requestId).toBe(42n);
	expect(decoded.trackAlias).toBe(43n);
});

test("SubscribeError: round trip", async () => {
	const msg = new Subscribe.SubscribeError({ requestId: 123n, errorCode: 500, reasonPhrase: "Not found" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Subscribe.SubscribeError.decode, Version.DRAFT_14);

	expect(decoded.requestId).toBe(123n);
	expect(decoded.errorCode).toBe(500);
	expect(decoded.reasonPhrase).toBe("Not found");
});

test("Unsubscribe: round trip", async () => {
	const msg = new Subscribe.Unsubscribe({ requestId: 999n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Subscribe.Unsubscribe.decode, Version.DRAFT_14);

	expect(decoded.requestId).toBe(999n);
});

test("PublishDone: basic test", async () => {
	const msg = new PublishDone({ requestId: 10n, statusCode: 0, reasonPhrase: "complete" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, PublishDone.decode, Version.DRAFT_14);

	expect(decoded.requestId).toBe(10n);
	expect(decoded.statusCode).toBe(0);
	expect(decoded.reasonPhrase).toBe("complete");
});

test("PublishDone: with error", async () => {
	const msg = new PublishDone({ requestId: 10n, statusCode: 1, reasonPhrase: "error" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, PublishDone.decode, Version.DRAFT_14);

	expect(decoded.requestId).toBe(10n);
	expect(decoded.statusCode).toBe(1);
	expect(decoded.reasonPhrase).toBe("error");
});

// Announce/PublishNamespace tests
test("PublishNamespace: round trip", async () => {
	const msg = new Announce.PublishNamespace({ requestId: 1n, trackNamespace: Path.from("test/broadcast") });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespace.decode, Version.DRAFT_14);

	expect(decoded.requestId).toBe(1n);
	expect(decoded.trackNamespace).toBe("test/broadcast" as Path.Valid);
});

test("PublishNamespaceOk: round trip", async () => {
	const msg = new Announce.PublishNamespaceOk({ requestId: 2n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespaceOk.decode, Version.DRAFT_14);

	expect(decoded.requestId).toBe(2n);
});

test("PublishNamespaceError: round trip", async () => {
	const msg = new Announce.PublishNamespaceError({ requestId: 3n, errorCode: 404, reasonPhrase: "Unauthorized" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespaceError.decode, Version.DRAFT_14);

	expect(decoded.requestId).toBe(3n);
	expect(decoded.errorCode).toBe(404);
	expect(decoded.reasonPhrase).toBe("Unauthorized");
});

test("PublishNamespaceDone: round trip", async () => {
	const msg = new Announce.PublishNamespaceDone({ trackNamespace: Path.from("old/stream") });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespaceDone.decode, Version.DRAFT_14);

	expect(decoded.trackNamespace).toBe("old/stream" as Path.Valid);
});

test("PublishNamespaceCancel: round trip", async () => {
	const msg = new Announce.PublishNamespaceCancel({
		trackNamespace: Path.from("canceled"),
		errorCode: 1,
		reasonPhrase: "Shutdown",
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespaceCancel.decode, Version.DRAFT_14);

	expect(decoded.trackNamespace).toBe("canceled" as Path.Valid);
	expect(decoded.errorCode).toBe(1);
	expect(decoded.reasonPhrase).toBe("Shutdown");
});

// GoAway tests
test("GoAway: with URL", async () => {
	const msg = new GoAway.GoAway({ newSessionUri: "https://example.com/new" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, GoAway.GoAway.decode, Version.DRAFT_14);

	expect(decoded.newSessionUri).toBe("https://example.com/new");
});

test("GoAway: empty", async () => {
	const msg = new GoAway.GoAway({ newSessionUri: "" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, GoAway.GoAway.decode, Version.DRAFT_14);

	expect(decoded.newSessionUri).toBe("");
});

// Track tests
test("TrackStatusRequest: round trip", async () => {
	const msg = new Track.TrackStatusRequest({
		requestId: 0n,
		trackNamespace: Path.from("video/stream"),
		trackName: "main",
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Track.TrackStatusRequest.decode, Version.DRAFT_14);

	expect(decoded.requestId).toBe(0n);
	expect(decoded.trackNamespace).toBe("video/stream" as Path.Valid);
	expect(decoded.trackName).toBe("main");
});

test("TrackStatus v14: round trip", async () => {
	const msg = new Track.TrackStatus({
		trackNamespace: Path.from("test"),
		trackName: "status",
		statusCode: 200,
		lastGroupId: 42n,
		lastObjectId: 100n,
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Track.TrackStatus.decode, Version.DRAFT_14);

	expect(decoded.trackNamespace).toBe("test" as Path.Valid);
	expect(decoded.trackName).toBe("status");
	expect(decoded.statusCode).toBe(200);
	expect(decoded.lastGroupId).toBe(42n);
	expect(decoded.lastObjectId).toBe(100n);
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

	await expect(
		(async () => {
			await decodeVersioned(invalidBytes, Subscribe.Subscribe.decode, Version.DRAFT_14);
		})(),
	).rejects.toThrow();
});

test("SubscribeOk v14: rejects non-zero expires", async () => {
	const invalidBytes = new Uint8Array([
		0x01, // subscribe_id
		0x05, // INVALID: expires = 5
		0x02, // group_order
		0x00, // content_exists
		0x00, // num_params
	]);

	await expect(
		(async () => {
			await decodeVersioned(invalidBytes, Subscribe.SubscribeOk.decode, Version.DRAFT_14);
		})(),
	).rejects.toThrow();
});

// Unicode tests
test("SubscribeError: unicode strings", async () => {
	const msg = new Subscribe.SubscribeError({ requestId: 1n, errorCode: 400, reasonPhrase: "Error: 错误 🚫" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Subscribe.SubscribeError.decode, Version.DRAFT_14);

	expect(decoded.requestId).toBe(1n);
	expect(decoded.errorCode).toBe(400);
	expect(decoded.reasonPhrase).toBe("Error: 错误 🚫");
});

test("PublishNamespace: unicode namespace", async () => {
	const msg = new Announce.PublishNamespace({ requestId: 1n, trackNamespace: Path.from("会议/房间") });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespace.decode, Version.DRAFT_14);

	expect(decoded.requestId).toBe(1n);
	expect(decoded.trackNamespace).toBe("会议/房间" as Path.Valid);
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

	expect(decoded.requestId).toBe(1n);
	expect(decoded.trackNamespace).toBe("test/ns" as Path.Valid);
	expect(decoded.trackName).toBe("video");
	expect(decoded.trackAlias).toBe(42n);
	expect(decoded.forward).toBe(true);
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

	expect(decoded.requestId).toBe(1n);
	expect(decoded.trackNamespace).toBe("test/ns" as Path.Valid);
	expect(decoded.trackName).toBe("video");
	expect(decoded.trackAlias).toBe(42n);
	expect(decoded.forward).toBe(true);
	expect(decoded.contentExists).toBe(true);
	expect(decoded.largest?.groupId).toBe(10n);
	expect(decoded.largest?.objectId).toBe(5n);
});

// ClientSetup v15 tests
test("ClientSetup v15: round trip", async () => {
	const msg = new Setup.ClientSetup({ versions: [Version.DRAFT_15] });

	const encoded = await encodeVersioned(msg, Version.DRAFT_15);
	const decoded = await decodeVersioned(encoded, Setup.ClientSetup.decode, Version.DRAFT_15);

	expect(decoded.versions.length).toBe(1);
	expect(decoded.versions[0]).toBe(Version.DRAFT_15);
});

test("ClientSetup v14: round trip", async () => {
	const msg = new Setup.ClientSetup({ versions: [Version.DRAFT_14] });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Setup.ClientSetup.decode, Version.DRAFT_14);

	expect(decoded.versions.length).toBe(1);
	expect(decoded.versions[0]).toBe(Version.DRAFT_14);
});

// ServerSetup v15 tests
test("ServerSetup v15: round trip", async () => {
	const msg = new Setup.ServerSetup({ version: Version.DRAFT_15 });

	const encoded = await encodeVersioned(msg, Version.DRAFT_15);
	const decoded = await decodeVersioned(encoded, Setup.ServerSetup.decode, Version.DRAFT_15);

	expect(decoded.version).toBe(Version.DRAFT_15);
});

test("ServerSetup v14: round trip", async () => {
	const msg = new Setup.ServerSetup({ version: Version.DRAFT_14 });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Setup.ServerSetup.decode, Version.DRAFT_14);

	expect(decoded.version).toBe(Version.DRAFT_14);
});

// RequestOk / RequestError v15 tests
test("RequestOk: round trip", async () => {
	const msg = new RequestOk({ requestId: 42n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_15);
	const decoded = await decodeVersioned(encoded, RequestOk.decode, Version.DRAFT_15);

	expect(decoded.requestId).toBe(42n);
});

test("RequestError v15: round trip", async () => {
	const msg = new RequestError({ requestId: 99n, errorCode: 500, reasonPhrase: "Internal error" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_15);
	const decoded = await decodeVersioned(encoded, RequestError.decode, Version.DRAFT_15);

	expect(decoded.requestId).toBe(99n);
	expect(decoded.errorCode).toBe(500);
	expect(decoded.reasonPhrase).toBe("Internal error");
	expect(decoded.retryInterval).toBe(0n);
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

	expect(decoded.requestId).toBe(99n);
	expect(decoded.errorCode).toBe(500);
	expect(decoded.reasonPhrase).toBe("Internal error");
	expect(decoded.retryInterval).toBe(5000n);
});

// --- Leading-ones varint tests ---

test("Leading-ones varint: spec test vectors", () => {
	// Test vectors from draft-17 spec (Table 2)
	const cases: [Uint8Array, bigint][] = [
		[new Uint8Array([0x25]), 37n],
		[new Uint8Array([0x80, 0x25]), 37n], // non-minimal encoding of 37
		[new Uint8Array([0xbb, 0xbd]), 15_293n],
		[new Uint8Array([0xfa, 0xa1, 0xa0, 0xe4, 0x03, 0xd8]), 2_893_212_287_960n],
		[new Uint8Array([0xfe, 0xfa, 0x31, 0x8f, 0xa8, 0xe3, 0xca, 0x11]), 70_423_237_261_249_041n],
		[
			new Uint8Array([0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
			18_446_744_073_709_551_615n, // u64::MAX
		],
	];

	for (const [bytes, expected] of cases) {
		const [decoded, remain] = Varint.decodeLeadingOnes(bytes);
		expect(decoded).toBe(expected);
		expect(remain.length).toBe(0);
	}

	// Test minimal round-trip (skip non-minimal 0x8025 for 37, and u64::MAX which exceeds safe range)
	const roundTripCases: [Uint8Array, bigint][] = [
		[new Uint8Array([0x25]), 37n],
		[new Uint8Array([0xbb, 0xbd]), 15_293n],
		[new Uint8Array([0xfa, 0xa1, 0xa0, 0xe4, 0x03, 0xd8]), 2_893_212_287_960n],
		[new Uint8Array([0xfe, 0xfa, 0x31, 0x8f, 0xa8, 0xe3, 0xca, 0x11]), 70_423_237_261_249_041n],
	];

	for (const [expectedBytes, value] of roundTripCases) {
		const encoded = Varint.encodeLeadingOnes(value);
		expect(encoded).toEqual(expectedBytes);
	}
});

test("Leading-ones varint: boundary round-trips", () => {
	const cases: [bigint, number][] = [
		[(1n << 7n) - 1n, 1],
		[1n << 7n, 2],
		[(1n << 14n) - 1n, 2],
		[1n << 14n, 3],
		[(1n << 56n) - 1n, 8],
		[1n << 56n, 9],
	];

	for (const [value, expectedLen] of cases) {
		const encoded = Varint.encodeLeadingOnes(value);
		expect(encoded.length).toBe(expectedLen);

		const [decoded, remain] = Varint.decodeLeadingOnes(encoded);
		expect(decoded).toBe(value);
		expect(remain.length).toBe(0);
	}
});

test("Leading-ones varint: invalid 0xFC prefix rejected", () => {
	expect(() => {
		Varint.decodeLeadingOnes(new Uint8Array([0xfc]));
	}).toThrow(/reserved/);
});

// --- Draft-17 message tests ---

test("Subscribe v17: round trip with requiredRequestIdDelta", async () => {
	const msg = new Subscribe.Subscribe({
		requestId: 1n,
		trackNamespace: Path.from("test"),
		trackName: "video",
		subscriberPriority: 128,
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_17);
	const decoded = await decodeVersioned(encoded, Subscribe.Subscribe.decode, Version.DRAFT_17);

	expect(decoded.requestId).toBe(1n);
	expect(decoded.trackNamespace).toBe("test" as Path.Valid);
	expect(decoded.trackName).toBe("video");
	expect(decoded.subscriberPriority).toBe(128);
});

test("SubscribeOk v17: no requestId", async () => {
	const msg = new Subscribe.SubscribeOk({ trackAlias: 42n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_17);
	const decoded = await decodeVersioned(encoded, Subscribe.SubscribeOk.decode, Version.DRAFT_17);

	expect(decoded.requestId).toBe(undefined);
	expect(decoded.trackAlias).toBe(42n);
});

test("RequestOk v17: no requestId", async () => {
	const msg = new RequestOk({});

	const encoded = await encodeVersioned(msg, Version.DRAFT_17);
	const decoded = await decodeVersioned(encoded, RequestOk.decode, Version.DRAFT_17);

	expect(decoded.requestId).toBe(undefined);
});

test("RequestError v17: no requestId, has retryInterval", async () => {
	const msg = new RequestError({
		errorCode: 500,
		reasonPhrase: "Internal error",
		retryInterval: 3000n,
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_17);
	const decoded = await decodeVersioned(encoded, RequestError.decode, Version.DRAFT_17);

	expect(decoded.requestId).toBe(undefined);
	expect(decoded.errorCode).toBe(500);
	expect(decoded.reasonPhrase).toBe("Internal error");
	expect(decoded.retryInterval).toBe(3000n);
});

test("GoAway v17: with timeout", async () => {
	const msg = new GoAway.GoAway({ newSessionUri: "https://example.com/new", timeout: 5000n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_17);
	const decoded = await decodeVersioned(encoded, GoAway.GoAway.decode, Version.DRAFT_17);

	expect(decoded.newSessionUri).toBe("https://example.com/new");
	expect(decoded.timeout).toBe(5000n);
});

test("Setup v17: unified 0x2F00 round trip", async () => {
	const params = new SetupOptions();
	params.setBytes(7n, new TextEncoder().encode("test-impl"));
	const msg = new Setup.Setup({ parameters: params });

	const encoded = await encodeVersioned(msg, Version.DRAFT_17);
	const decoded = await decodeVersioned(encoded, Setup.Setup.decode, Version.DRAFT_17);

	expect(decoded.parameters.getBytes(7n)).toEqual(new TextEncoder().encode("test-impl"));
});

test("Publish v17: round trip with requiredRequestIdDelta", async () => {
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

	const encoded = await encodeVersioned(msg, Version.DRAFT_17);
	const decoded = await decodeVersioned(encoded, Publish.decode, Version.DRAFT_17);

	expect(decoded.requestId).toBe(1n);
	expect(decoded.trackNamespace).toBe("test/ns" as Path.Valid);
	expect(decoded.trackName).toBe("video");
	expect(decoded.trackAlias).toBe(42n);
	expect(decoded.forward).toBe(true);
});

test("PublishNamespace v17: round trip", async () => {
	const msg = new Announce.PublishNamespace({ requestId: 5n, trackNamespace: Path.from("live/stream") });

	const encoded = await encodeVersioned(msg, Version.DRAFT_17);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespace.decode, Version.DRAFT_17);

	expect(decoded.requestId).toBe(5n);
	expect(decoded.trackNamespace).toBe("live/stream" as Path.Valid);
});

test("PublishNamespaceDone v17: encode rejects", async () => {
	const msg = new Announce.PublishNamespaceDone({ trackNamespace: Path.from("old/stream") });

	await expect((() => encodeVersioned(msg, Version.DRAFT_17))()).rejects.toThrow(/removed in draft-17/);
});

test("PublishNamespaceCancel v17: encode rejects", async () => {
	const msg = new Announce.PublishNamespaceCancel({ trackNamespace: Path.from("canceled") });

	await expect((() => encodeVersioned(msg, Version.DRAFT_17))()).rejects.toThrow(/removed in draft-17/);
});

test("PublishDone v17: no requestId", async () => {
	const msg = new PublishDone({ statusCode: 0, reasonPhrase: "done" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_17);
	const decoded = await decodeVersioned(encoded, PublishDone.decode, Version.DRAFT_17);

	expect(decoded.requestId).toBe(undefined);
	expect(decoded.statusCode).toBe(0);
	expect(decoded.reasonPhrase).toBe("done");
});

// --- SubscribeUpdate tests ---

test("SubscribeUpdate v14: round trip", async () => {
	const msg = new Subscribe.SubscribeUpdate({ requestId: 5n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Subscribe.SubscribeUpdate.decode, Version.DRAFT_14);

	expect(decoded.requestId).toBe(5n);
});

test("SubscribeUpdate v15: round trip", async () => {
	const msg = new Subscribe.SubscribeUpdate({ requestId: 10n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_15);
	const decoded = await decodeVersioned(encoded, Subscribe.SubscribeUpdate.decode, Version.DRAFT_15);

	expect(decoded.requestId).toBe(10n);
});

test("SubscribeUpdate v17: round trip with requiredRequestIdDelta", async () => {
	const msg = new Subscribe.SubscribeUpdate({ requestId: 42n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_17);
	const decoded = await decodeVersioned(encoded, Subscribe.SubscribeUpdate.decode, Version.DRAFT_17);

	expect(decoded.requestId).toBe(42n);
});

// --- PublishBlocked tests ---

test("PublishBlocked v17: round trip", async () => {
	const msg = new SubscribeNamespace.PublishBlocked({
		suffix: Path.from("stream1"),
		trackName: "video",
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_17);
	const decoded = await decodeVersioned(encoded, SubscribeNamespace.PublishBlocked.decode, Version.DRAFT_17);

	expect(decoded.suffix).toBe("stream1" as Path.Valid);
	expect(decoded.trackName).toBe("video");
});

// --- TrackStatusRequest version-aware tests ---

test("TrackStatusRequest v14: round trip with subscribe fields", async () => {
	const msg = new Track.TrackStatusRequest({
		requestId: 1n,
		trackNamespace: Path.from("video/stream"),
		trackName: "main",
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_14);
	const decoded = await decodeVersioned(encoded, Track.TrackStatusRequest.decode, Version.DRAFT_14);

	expect(decoded.requestId).toBe(1n);
	expect(decoded.trackNamespace).toBe("video/stream" as Path.Valid);
	expect(decoded.trackName).toBe("main");
});

test("TrackStatusRequest v15: round trip with params", async () => {
	const msg = new Track.TrackStatusRequest({ requestId: 2n, trackNamespace: Path.from("test"), trackName: "audio" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_15);
	const decoded = await decodeVersioned(encoded, Track.TrackStatusRequest.decode, Version.DRAFT_15);

	expect(decoded.requestId).toBe(2n);
	expect(decoded.trackNamespace).toBe("test" as Path.Valid);
	expect(decoded.trackName).toBe("audio");
});

test("TrackStatusRequest v17: round trip with requiredRequestIdDelta", async () => {
	const msg = new Track.TrackStatusRequest({ requestId: 3n, trackNamespace: Path.from("live"), trackName: "data" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_17);
	const decoded = await decodeVersioned(encoded, Track.TrackStatusRequest.decode, Version.DRAFT_17);

	expect(decoded.requestId).toBe(3n);
	expect(decoded.trackNamespace).toBe("live" as Path.Valid);
	expect(decoded.trackName).toBe("data");
});
