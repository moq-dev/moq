import { expect, test } from "bun:test";
import { Milli } from "@moq/clock";
import { ProtocolViolation } from "../error.ts";
import * as Path from "../path.ts";
import { type Cursor, Reader, Writer } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import * as Varint from "../varint.ts";
import { Fetch } from "./fetch.ts";
import * as GoAway from "./goaway.ts";
import * as Namespace from "./namespace.ts";
import {
	EndOfRange,
	FetchEndOfRange,
	FetchFrame,
	type FetchPosition,
	Frame,
	Group,
	type GroupFlags,
} from "./object.ts";
import { Parameters } from "./parameters.ts";
import { Publish, PublishDone } from "./publish.ts";
import * as Announce from "./publish_namespace.ts";
import { RequestError, RequestOk } from "./request.ts";
import * as Setup from "./setup.ts";
import { SetupOptions } from "./setup_options.ts";
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

async function encodeFrameVersioned(
	frame: Frame,
	flags: GroupFlags,
	version: IetfVersion,
	timescale: Timescale = Timescale.MILLI,
): Promise<Uint8Array> {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream, version);
	await frame.encode(writer, flags, timescale, version);
	writer.close();
	await writer.closed;
	return concatChunks(written);
}

async function encodeFetchFrameVersioned(
	frame: FetchFrame,
	position: FetchPosition,
	version: IetfVersion,
	timescale: Timescale = Timescale.MILLI,
): Promise<Uint8Array> {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream, version);
	await frame.encode(writer, position, timescale, version);
	writer.close();
	await writer.closed;
	return concatChunks(written);
}

test("DEFAULT_PUBLISHER_PRIORITY has exact SUBSCRIBE_OK bytes per draft", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22] as const) {
		const baseline = await encodeVersioned(new Subscribe.SubscribeOk({ trackAlias: 42n }), version);
		const encoded = await encodeVersioned(
			new Subscribe.SubscribeOk({ trackAlias: 42n, properties: { priority: 37 } }),
			version,
		);
		expect(Array.from(encoded)).toEqual([baseline[0], baseline[1] + 2, ...baseline.slice(2), 0x0e, 37]);
		const decoded = await decodeVersioned(encoded, Subscribe.SubscribeOk.decode, version);
		expect(decoded.properties.priority).toBe(37);
	}
});

test("DEFAULT_PUBLISHER_PRIORITY has exact PUBLISH bytes per draft", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22] as const) {
		const fields = {
			requestId: 1n,
			trackNamespace: Path.from("ns"),
			trackName: "video",
			trackAlias: 42n,
			groupOrder: 2,
			contentExists: false,
			largest: undefined,
			forward: true,
		};
		const baseline = await encodeVersioned(new Publish(fields), version);
		const encoded = await encodeVersioned(new Publish({ ...fields, priority: 37 }), version);
		// The new property precedes GROUP_ORDER, so its delta changes 0x22 to 0x14.
		expect(Array.from(encoded)).toEqual([
			baseline[0],
			baseline[1] + 2,
			...baseline.slice(2, -2),
			0x0e,
			37,
			0x14,
			2,
		]);
		const decoded = await decodeVersioned(encoded, Publish.decode, version);
		expect(decoded.priority).toBe(37);
	}
});

test("Message Parameters: Location preserves bigint precision", async () => {
	const largest = { groupId: 9007199254740993n, objectId: 9007199254740995n };
	const params = new Parameters();
	params.largest = largest;

	expect(params.largest).toEqual(largest);

	for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22]) {
		const encoded = await encodeVersioned(params, version);
		const decoded = await decodeVersioned(encoded, Parameters.decode, version);
		expect(decoded.largest).toEqual(largest);
	}
});

test("Message Parameters: Location preserves full uint64 values", async () => {
	const largest = { groupId: 2n ** 64n - 1n, objectId: 2n ** 62n };
	const params = new Parameters();
	params.largest = largest;

	expect(params.largest).toEqual(largest);

	for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22]) {
		const encoded = await encodeVersioned(params, version);
		const decoded = await decodeVersioned(encoded, Parameters.decode, version);
		expect(decoded.largest).toEqual(largest);
	}
});

test("PublishDone: preserves unknown statuses across supported versions", async () => {
	const versions = [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22] as const;

	for (const version of versions) {
		const msg = new PublishDone({
			statusCode: 0xface,
			reasonPhrase: "extension",
		});

		const encoded = await encodeVersioned(msg, version);
		const decoded = await decodeVersioned(encoded, PublishDone.decode, version);
		expect(decoded.statusCode).toBe(0xface);
	}
});

// TRACK_STATUS_OK carries what SUBSCRIBE_OK would.
test("TrackStatusOk: round trip on every draft", async () => {
	const properties = { timescale: Timescale.MILLI, priority: 7, groupOrder: 0x02, maxCacheDuration: 1000n };
	for (const version of Object.values(Version)) {
		const msg = new Track.TrackStatusOk({
			largest: { groupId: 4n, objectId: 1n },
			properties,
		});
		const encoded = await encodeVersioned(msg, version);
		const decoded = await decodeVersioned(encoded, Track.TrackStatusOk.decode, version);

		expect(decoded.largest).toEqual({ groupId: 4n, objectId: 1n });
		expect(decoded.properties).toEqual(properties);
	}
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

test("Leading-ones varint: 0xFC is a 7-byte form on draft-18+ (per #1595)", () => {
	// Only check that it doesn't reject the prefix as reserved: it needs more bytes.
	expect(() => {
		Varint.decodeLeadingOnes(new Uint8Array([0xfc]));
	}).toThrow(/buffer too short/);
});

test("Reader#u62: accepts 0xFC 7-byte form on draft-18", async () => {
	const value = 0x12_3456_789a_bcn;
	const bytes = new Uint8Array([0xfc, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
	const reader = new Reader(undefined, bytes, Version.DRAFT_18);
	expect(await reader.u62()).toBe(value);
	expect(await reader.done()).toBe(true);
});

test("Reader#u62: accepts 0xFC 7-byte form on draft-22", async () => {
	const value = 0x12_3456_789a_bcn;
	const bytes = new Uint8Array([0xfc, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
	const reader = new Reader(undefined, bytes, Version.DRAFT_22);
	expect(await reader.u62()).toBe(value);
	expect(await reader.done()).toBe(true);
});

test("Leading-ones varint: 7-byte form round-trips on draft-18", () => {
	// Encode 7 bytes manually: 1111110_0 (prefix bit 48 = 0) + 6 bytes payload
	const value = 0x12_3456_789a_bcn;
	const bytes = new Uint8Array(7);
	bytes[0] = 0xfc; // 1111110_0
	bytes[1] = 0x12;
	bytes[2] = 0x34;
	bytes[3] = 0x56;
	bytes[4] = 0x78;
	bytes[5] = 0x9a;
	bytes[6] = 0xbc;
	const [decoded, remain] = Varint.decodeLeadingOnes(bytes);
	expect(decoded).toBe(value);
	expect(remain.length).toBe(0);
});

test("GoAway v18: carries the Request ID", async () => {
	const msg = new GoAway.GoAway({ newSessionUri: "moqt://relay.example/", timeout: 5000n, requestId: 1n });

	// Draft-18 (#1559) requires a trailing Request ID on the control stream.
	const encoded = await encodeVersioned(msg, Version.DRAFT_18);
	const decoded = await decodeVersioned(encoded, GoAway.GoAway.decode, Version.DRAFT_18);
	expect(decoded.newSessionUri).toBe("moqt://relay.example/");
	expect(decoded.timeout).toBe(5000n);
	expect(decoded.requestId).toBe(1n);
});

test("GoAway v22: round trip without the Request ID", async () => {
	// Draft-22 has no Request ID: the body is just URI + timeout.
	const msg = new GoAway.GoAway({ newSessionUri: "moqt://relay.example/", timeout: 5000n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_22);
	const decoded = await decodeVersioned(encoded, GoAway.GoAway.decode, Version.DRAFT_22);

	expect(decoded.newSessionUri).toBe("moqt://relay.example/");
	expect(decoded.timeout).toBe(5000n);
});

test("Setup: unified 0x2F00 round trip", async () => {
	const params = new SetupOptions();
	params.setBytes(7n, new TextEncoder().encode("test-impl"));
	const msg = new Setup.Setup({ parameters: params });

	const encoded = await encodeVersioned(msg, Version.DRAFT_18);
	const decoded = await decodeVersioned(encoded, Setup.Setup.decode, Version.DRAFT_18);

	expect(decoded.parameters.getBytes(7n)).toEqual(new TextEncoder().encode("test-impl"));
});

test("SubscribeUpdate v18: round trip", async () => {
	const msg = new Subscribe.SubscribeUpdate({ requestId: 42n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_18);
	const reader = new Reader(undefined, encoded, Version.DRAFT_18);
	await reader.u16();
	expect(await reader.u62()).toBe(42n);

	const decoded = await decodeVersioned(encoded, Subscribe.SubscribeUpdate.decode, Version.DRAFT_18);

	expect(decoded.requestId).toBe(42n);
});

// --- TrackStatusRequest version-aware tests ---

test("TrackStatusRequest v18: round trip", async () => {
	const msg = new Track.TrackStatusRequest({ requestId: 3n, trackNamespace: Path.from("live"), trackName: "data" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_18);
	const decoded = await decodeVersioned(encoded, Track.TrackStatusRequest.decode, Version.DRAFT_18);

	expect(decoded.requestId).toBe(3n);
	expect(decoded.trackNamespace).toBe("live" as Path.Valid);
	expect(decoded.trackName).toBe("data");
});

// --- Namespace encoding tests ---

// Helper to encode a namespace to raw bytes
async function encodeNamespace(namespace: Path.Valid): Promise<Uint8Array> {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream, Version.DRAFT_18);
	await Namespace.encode(writer, namespace);
	writer.close();
	await writer.closed;
	return concatChunks(written);
}

// Helper to decode a namespace from raw bytes
async function decodeNamespace(bytes: Uint8Array): Promise<Path.Valid> {
	const reader = new Reader(undefined, bytes, Version.DRAFT_18);
	return await Namespace.decode(reader);
}

// Helper to encode raw IETF namespace tuple fields
async function encodeNamespaceTuple(parts: string[]): Promise<Uint8Array> {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream, Version.DRAFT_18);
	await writer.u53(parts.length);
	for (const part of parts) await writer.string(part);
	writer.close();
	await writer.closed;
	return concatChunks(written);
}

test("Namespace: empty encodes as zero-length tuple", async () => {
	const bytes = await encodeNamespace(Path.empty());

	// Should be a single byte: varint 0 (zero parts)
	expect(bytes).toEqual(new Uint8Array([0x00]));
});

test("Namespace: empty round trip", async () => {
	const encoded = await encodeNamespace(Path.empty());
	const decoded = await decodeNamespace(encoded);

	expect(decoded).toBe("" as Path.Valid);
});

test("Namespace: single part round trip", async () => {
	const encoded = await encodeNamespace(Path.from("test"));
	const decoded = await decodeNamespace(encoded);

	expect(decoded).toBe("test" as Path.Valid);
});

test("Namespace: single part encodes as one-length tuple", async () => {
	const bytes = await encodeNamespace(Path.from("test"));

	// varint 1 (one part), then length-prefixed "test"
	expect(bytes[0]).toBe(0x01);
});

test("Namespace: multi-part round trip", async () => {
	const encoded = await encodeNamespace(Path.from("conference/room/123"));
	const decoded = await decodeNamespace(encoded);

	expect(decoded).toBe("conference/room/123" as Path.Valid);
});

test("Namespace: multi-part encodes correct count", async () => {
	const bytes = await encodeNamespace(Path.from("a/b/c"));

	// First byte should be varint 3 (three parts)
	expect(bytes[0]).toBe(0x03);
});

test("Namespace: slash in tuple part is escaped", async () => {
	const tuple = await encodeNamespaceTuple(["foo/bar", "baz"]);
	const decoded = await decodeNamespace(tuple);

	expect(decoded).toBe("foo\\/bar/baz" as Path.Valid);
	expect(await encodeNamespace(decoded)).toEqual(tuple);
});

test("Namespace: literal backslash round trips", async () => {
	const tuple = await encodeNamespaceTuple(["foo\\bar/baz", "qux"]);
	const decoded = await decodeNamespace(tuple);

	expect(decoded).toBe("foo\\\\bar\\/baz/qux" as Path.Valid);
	expect(await encodeNamespace(decoded)).toEqual(tuple);
});

test("Namespace: slashes at tuple part boundaries", async () => {
	// A leading slash escapes to a non-empty `\` segment.
	const tuple = await encodeNamespaceTuple(["/foo", "/bar"]);
	const decoded = await decodeNamespace(tuple);
	expect(decoded).toBe("\\/foo/\\/bar" as Path.Valid);
	expect(await encodeNamespace(decoded)).toEqual(tuple);

	// A trailing slash escapes to an empty segment, which every later path operation assumes away.
	for (const fields of [["/foo", "bar/", "/baz/"], ["foo/", "bar"], ["/"], ["a//"]]) {
		await expect(decodeNamespace(await encodeNamespaceTuple(fields))).rejects.toThrow(ProtocolViolation);
	}
});

test("Namespace: Subscribe with empty namespace round trip", async () => {
	const msg = new Subscribe.Subscribe({
		requestId: 1n,
		trackNamespace: Path.empty(),
		trackName: "video",
		subscriberPriority: 128,
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_18);
	const decoded = await decodeVersioned(encoded, Subscribe.Subscribe.decode, Version.DRAFT_18);

	expect(decoded.trackNamespace).toBe("" as Path.Valid);
	expect(decoded.trackName).toBe("video");
});

test("Namespace: PublishNamespace with empty namespace round trip", async () => {
	const msg = new Announce.PublishNamespace({ requestId: 1n, trackNamespace: Path.empty() });

	const encoded = await encodeVersioned(msg, Version.DRAFT_18);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespace.decode, Version.DRAFT_18);

	expect(decoded.trackNamespace).toBe("" as Path.Valid);
});

// --- Draft-18 message tests ---

test("Subscribe v18: round trip", async () => {
	const msg = new Subscribe.Subscribe({
		requestId: 1n,
		trackNamespace: Path.from("test"),
		trackName: "video",
		subscriberPriority: 128,
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_18);
	const decoded = await decodeVersioned(encoded, Subscribe.Subscribe.decode, Version.DRAFT_18);

	expect(decoded.requestId).toBe(1n);
	expect(decoded.trackNamespace).toBe("test" as Path.Valid);
	expect(decoded.trackName).toBe("video");
	expect(decoded.subscriberPriority).toBe(128);
});

test("SubscribeOk v18: no requestId", async () => {
	const msg = new Subscribe.SubscribeOk({ trackAlias: 42n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_18);
	const decoded = await decodeVersioned(encoded, Subscribe.SubscribeOk.decode, Version.DRAFT_18);

	expect(decoded.trackAlias).toBe(42n);
});

// The regression from #3558: a draft-18 peer sends the Group as a two-byte varint, and reading
// the parameter as length-prefixed turned that first byte into a demand for 427 bytes of value,
// failing the message on a short buffer.
test("SubscribeOk v18: reads a multi-byte LARGEST_OBJECT group", async () => {
	const body = [
		0x04, // track alias
		0x01, // one message parameter
		0x09, // LARGEST_OBJECT
		0x81,
		0xab, // group 427, a two-byte leading-ones varint
		0x00, // object 0
	];
	const bytes = new Uint8Array([0x00, body.length, ...body]);

	const decoded = await decodeVersioned(bytes, Subscribe.SubscribeOk.decode, Version.DRAFT_18);
	expect(decoded.largest).toEqual({ groupId: 427n, objectId: 0n });
});

// GROUP_ORDER (0x22) is not a SUBSCRIBE_OK *message parameter*; a peer closes the session with
// PROTOCOL_VIOLATION when it sees one. The publisher's preference belongs in the DEFAULT_PUBLISHER_GROUP_ORDER track property, which shares the
// number 0x22 in the separate property registry.
test("SubscribeOk v18: group order is a track property, not a parameter", async () => {
	const msg = new Subscribe.SubscribeOk({ trackAlias: 42n, properties: { groupOrder: 0x02 } });

	const encoded = await encodeVersioned(msg, Version.DRAFT_18);
	expect(Array.from(encoded)).toEqual([
		0x00,
		0x04, // u16 message length
		42, // track alias
		0x00, // zero message parameters
		0x22, // DEFAULT_PUBLISHER_GROUP_ORDER, the first (and only) track property
		0x02, // descending
	]);
});

// Only Ascending and Descending are defined, so 0x0 is a malformed message.
test("SubscribeOk v18: rejects a zero group order property", async () => {
	const body = [
		42, // track alias
		0x00, // zero message parameters
		0x22, // DEFAULT_PUBLISHER_GROUP_ORDER
		0x00, // invalid
	];
	const bytes = new Uint8Array([0x00, body.length, ...body]);

	await expect(decodeVersioned(bytes, Subscribe.SubscribeOk.decode, Version.DRAFT_18)).rejects.toThrow(
		"unknown group order",
	);
});

test("PublishNamespace v18: round trip", async () => {
	const msg = new Announce.PublishNamespace({ requestId: 5n, trackNamespace: Path.from("v18/broadcast") });
	const encoded = await encodeVersioned(msg, Version.DRAFT_18);
	const decoded = await decodeVersioned(encoded, Announce.PublishNamespace.decode, Version.DRAFT_18);
	expect(decoded.requestId).toBe(5n);
	expect(decoded.trackNamespace).toBe("v18/broadcast" as Path.Valid);
});

test("Publish v18: round trip", async () => {
	const msg = new Publish({
		requestId: 7n,
		trackNamespace: Path.from("v18/ns"),
		trackName: "video",
		trackAlias: 1n,
		groupOrder: 2,
		contentExists: false,
		largest: undefined,
		forward: true,
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_18);
	const decoded = await decodeVersioned(encoded, Publish.decode, Version.DRAFT_18);

	expect(decoded.requestId).toBe(7n);
	expect(decoded.trackNamespace).toBe("v18/ns" as Path.Valid);
	expect(decoded.trackName).toBe("video");
	expect(decoded.trackAlias).toBe(1n);
	expect(decoded.forward).toBe(true);
});

test("PublishDone v18: no requestId", async () => {
	const msg = new PublishDone({ statusCode: 2, reasonPhrase: "track ended" });

	const encoded = await encodeVersioned(msg, Version.DRAFT_18);
	const decoded = await decodeVersioned(encoded, PublishDone.decode, Version.DRAFT_18);

	expect(decoded.statusCode).toBe(2);
	expect(decoded.reasonPhrase).toBe("track ended");
});

test("RequestOk v18: round trip", async () => {
	const msg = new RequestOk({});

	const encoded = await encodeVersioned(msg, Version.DRAFT_18);
	const decoded = await decodeVersioned(encoded, RequestOk.decode, Version.DRAFT_18);
	expect(decoded.parameters.largest).toBeUndefined();
});

// A REQUEST_UPDATE_OK or TRACK_STATUS_OK must carry LARGEST_OBJECT once the track has
// objects, so a REQUEST_OK with one decodes.
test("RequestOk: accepts LARGEST_OBJECT", async () => {
	const versions = [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22] as const;
	for (const version of versions) {
		const body = [0x01, 0x09, 0x05, 0x03];
		const bytes = new Uint8Array([0x00, body.length, ...body]);
		const decoded = await decodeVersioned(bytes, RequestOk.decode, version);
		expect(decoded.parameters.largest).toEqual({ groupId: 5n, objectId: 3n });
	}
});

test("RequestError v18: no requestId, retry_interval still present", async () => {
	const msg = new RequestError({
		errorCode: 500,
		reasonPhrase: "boom",
		retryInterval: 1234n,
	});

	const encoded = await encodeVersioned(msg, Version.DRAFT_18);
	const decoded = await decodeVersioned(encoded, RequestError.decode, Version.DRAFT_18);

	expect(decoded.errorCode).toBe(500);
	expect(decoded.reasonPhrase).toBe("boom");
	expect(decoded.retryInterval).toBe(1234n);
});

test("GoAway v18: round trip (timeout field still present)", async () => {
	const msg = new GoAway.GoAway({ newSessionUri: "moqt://relay.example/", timeout: 5000n });

	const encoded = await encodeVersioned(msg, Version.DRAFT_22);
	const decoded = await decodeVersioned(encoded, GoAway.GoAway.decode, Version.DRAFT_22);

	expect(decoded.newSessionUri).toBe("moqt://relay.example/");
	expect(decoded.timeout).toBe(5000n);
});

test("SubscribeNamespace: message ID matches the draft registry", () => {
	expect(SubscribeNamespace.SubscribeNamespace.id).toBe(0x50);
});

test("SubscribeNamespace: round trip", async () => {
	const m = await encodeVersioned(
		new SubscribeNamespace.SubscribeNamespace({ namespace: Path.from("example/meeting"), requestId: 4n }),
		Version.DRAFT_18,
	);
	const dm = await decodeVersioned(m, SubscribeNamespace.SubscribeNamespace.decode, Version.DRAFT_18);
	expect(dm.requestId).toBe(4n);
	expect(dm.namespace).toBe("example/meeting" as Path.Valid);
});

// FORWARD is not a SUBSCRIBE_NAMESPACE parameter, so carrying one closes the session.
test("SubscribeNamespace: FORWARD is rejected", async () => {
	await expect(
		decodeVersioned(
			framed([0x01, 0x00, 0x01, 0x10, 0x00]),
			SubscribeNamespace.SubscribeNamespace.decode,
			Version.DRAFT_18,
		),
	).rejects.toThrow(ProtocolViolation);
});

// TRACK_NAMESPACE_PREFIX updates a SUBSCRIBE_NAMESPACE or SUBSCRIBE_TRACKS only, so a
// subscription's REQUEST_UPDATE carrying it closes the session instead of being acked.
test("Parameters: TRACK_NAMESPACE_PREFIX is not a subscription update parameter", async () => {
	// One parameter: TRACK_NAMESPACE_PREFIX (0x34), a one-field namespace "a".
	const block = new Uint8Array([0x01, 0x34, 0x01, 0x01, 0x61]);
	await expect(
		Parameters.decode(new Reader(undefined, block, Version.DRAFT_18), Version.DRAFT_18, "request-update"),
	).rejects.toThrow(ProtocolViolation);
});

// FIRST_OBJECT says the stream carries the subgroup from its first published object. It is
// the only signal that a group arrived with its head missing, so the value has to survive
// decode rather than being stripped along with the bit.
test("group flags round-trip firstObject", async () => {
	const makeGroup = (firstObject: boolean) =>
		new Group({
			trackAlias: 7n,
			groupId: 3,
			subGroupId: 0,
			publisherPriority: 0,
			flags: {
				hasExtensions: false,
				hasSubgroup: false,
				hasSubgroupObject: false,
				hasEnd: true,
				hasPriority: true,
				firstObject,
			},
		});

	for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22]) {
		for (const firstObject of [true, false]) {
			const encoded = await encodeVersioned(makeGroup(firstObject), version);
			const decoded = await decodeVersioned(encoded, Group.decode, version);
			expect(decoded.flags.firstObject).toBe(firstObject);
		}
	}
});

// The End-of-Group header bit and an END_OF_GROUP (0x03) status are independent, so a
// publisher may send both. Either way the group ends on a payload-less frame.
test("Frame: END_OF_GROUP is accepted whether or not the header marks the end", async () => {
	const makeFlags = (hasEnd: boolean): GroupFlags => ({
		hasExtensions: false,
		hasSubgroup: false,
		hasSubgroupObject: false,
		hasEnd,
		hasPriority: true,
		firstObject: true,
	});

	// idDelta 0, zero-length payload, then the END_OF_GROUP status.
	const wire = new Uint8Array([0x00, 0x00, 0x03]);

	for (const hasEnd of [false, true]) {
		const decoded = await new Reader(undefined, wire, Version.DRAFT_22).decode((c) =>
			Frame.decode(c, makeFlags(hasEnd), Timescale.MILLI),
		);

		// No payload is the group-end signal the subscriber breaks its read loop on.
		expect(decoded.payload).toBeUndefined();
		expect(decoded.endOfTrack).toBe(false);
	}

	// Status 0 on a marked stream is still an empty frame, not a group end.
	const empty = await new Reader(undefined, new Uint8Array([0x00, 0x00, 0x00]), Version.DRAFT_22).decode((c) =>
		Frame.decode(c, makeFlags(true), Timescale.MILLI),
	);
	expect(empty.payload).toEqual(new Uint8Array(0));
	expect(empty.endOfTrack).toBe(false);
});

test("Frame decodes objects split at every byte", async () => {
	const flags: GroupFlags = {
		hasExtensions: true,
		hasSubgroup: false,
		hasSubgroupObject: false,
		hasEnd: true,
		hasPriority: true,
		firstObject: true,
	};
	const frames = [
		new Frame({ payload: new Uint8Array([1]), timestamp: new Timestamp(96_000, Timescale.MILLI) }),
		new Frame({ payload: new Uint8Array(300).fill(2), timestamp: new Timestamp(96_033, Timescale.MILLI) }),
	];
	const bytes = concatChunks(await Promise.all(frames.map((f) => encodeFrameVersioned(f, flags, Version.DRAFT_22))));
	const reader = new Reader(
		new ReadableStream<Uint8Array>({
			start(controller) {
				for (const byte of bytes) controller.enqueue(new Uint8Array([byte]));
				controller.close();
			},
		}),
		undefined,
		Version.DRAFT_22,
	);

	const decode = (c: Cursor) => Frame.decode(c, flags, Timescale.MILLI);
	for (const frame of frames) {
		const decoded = await reader.decodeMaybe(decode);
		expect(decoded?.payload).toEqual(frame.payload);
		expect(decoded?.timestamp?.value).toBe(frame.timestamp?.value);
	}
	expect(await reader.decodeMaybe(decode)).toBeUndefined();
});

// The properties block is sized on the wire, so a property running past it is malformed, not
// a reason to wait for more of the stream.
test("Frame rejects a property that runs past its block", async () => {
	const flags: GroupFlags = {
		hasExtensions: true,
		hasSubgroup: false,
		hasSubgroupObject: false,
		hasEnd: true,
		hasPriority: true,
		firstObject: true,
	};
	// Delta 0, a 2-byte block holding a Timestamp id and the first byte of a 2-byte value, a
	// 1-byte payload, then bytes the block must not borrow.
	const reader = new Reader(undefined, new Uint8Array([0, 2, 0x10, 0x80, 1, 0xaa, 0x01]), Version.DRAFT_22);
	await expect(reader.decode((c) => Frame.decode(c, flags, Timescale.MILLI))).rejects.toThrow(
		"message is shorter than its fields",
	);
});

test("Frame reads only the current LOC Timestamp ID on every draft", async () => {
	const flags: GroupFlags = {
		hasExtensions: true,
		hasSubgroup: false,
		hasSubgroupObject: false,
		hasEnd: true,
		hasPriority: true,
		firstObject: true,
	};
	for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22] as const) {
		for (const id of [0x06, 0x10]) {
			const reader = new Reader(undefined, new Uint8Array([0, 2, id, 42, 1, 0xaa]), version);
			const frame = await reader.decode((c) => Frame.decode(c, flags, Timescale.MILLI));
			expect(frame.timestamp?.value).toBe(id === 0x10 ? 42 : undefined);
		}
	}
});

// A fetch stream's first object is the only one carrying absolute ids, so a wrong flag byte
// there silently renumbers every object after it. This codec is the sole serialization path
// for a served fill.
test("FetchFrame: the first object carries its absolute ids and the rest inherit them", async () => {
	const timestamp = new Timestamp(96_000, Timescale.MILLI);
	const frame = new FetchFrame({ payload: new Uint8Array([0xaa, 0xbb]), timestamp });

	const first = await encodeFetchFrameVersioned(frame, { group: 7, object: 3, first: true }, Version.DRAFT_22);
	expect(Array.from(first)).toEqual([
		0x3c, // GROUP_ID | OBJECT_ID | PRIORITY | PROPERTIES
		7, // group id
		3, // object id
		0, // publisher priority
		0x04, // properties length
		0x10, // TIMESTAMP
		0xc1,
		0x77,
		0x00, // 96000 as a three byte leading-ones varint
		0x02, // payload length
		0xaa,
		0xbb,
	]);

	// Same group and priority, and the object id is the prior one plus one, so only the
	// properties and the payload go on the wire.
	const next = await encodeFetchFrameVersioned(frame, { group: 7, object: 4, first: false }, Version.DRAFT_22);
	expect(Array.from(next)).toEqual([
		0x20, // PROPERTIES
		0x04,
		0x10,
		0xc1,
		0x77,
		0x00,
		0x02,
		0xaa,
		0xbb,
	]);
});

// An End of Range names absolute ids wherever it falls and carries no payload length, as
// every implementation reads it (moq-wg/moq-transport#1861). Read as a delta, its group
// could never name the group it follows.
test("FetchEndOfRange: absolute ids and nothing after them", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22] as const) {
		const reason = version === Version.DRAFT_18 ? EndOfRange.Unknown : EndOfRange.TimedOut;
		const { stream, written } = createTestWritableStream();
		const writer = new Writer(stream, version);
		await new FetchEndOfRange({ reason, group: 7n, object: 2n ** 62n - 1n }).encode(writer, version);
		writer.close();
		await writer.closed;

		const reader = new Reader(undefined, concatChunks(written), version);
		expect(await reader.u53()).toBe(reason);
		expect(await reader.u62()).toBe(7n);
		expect(await reader.u62()).toBe(2n ** 62n - 1n);
		expect(await reader.done()).toBe(true);
	}

	// Draft-18 has no Timed-Out marker; a FILL_TIMEOUT there reports Unknown.
	const marker = new FetchEndOfRange({ reason: EndOfRange.TimedOut, group: 0n, object: 0n });
	const { stream } = createTestWritableStream();
	await expect(marker.encode(new Writer(stream, Version.DRAFT_18), Version.DRAFT_18)).rejects.toThrow();
});

// The first object after an End of Range has no prior object's priority to inherit, but its
// group is still a delta from the marker's.
test("FetchFrame: the first object after an End of Range", async () => {
	const frame = new FetchFrame({ payload: new Uint8Array([0x01]) });
	const encoded = await encodeFetchFrameVersioned(
		frame,
		{ group: 9, object: 0, first: true, previousGroup: 7 },
		Version.DRAFT_22,
	);
	// GROUP_ID | OBJECT_ID | PRIORITY, group 7 + 1 + 1, object 0, priority 0, then the payload.
	expect(Array.from(encoded)).toEqual([0x1c, 0x01, 0x00, 0x00, 0x01, 0x01]);
});

// A fetch object has no status field, unlike a subgroup object: a zero payload length is
// simply an empty object, and writing a status after it would desync the stream.
test("FetchFrame: an empty payload is a zero length, with no status byte", async () => {
	const frame = new FetchFrame({ payload: new Uint8Array(), timestamp: new Timestamp(0, Timescale.MILLI) });
	const encoded = await encodeFetchFrameVersioned(frame, { group: 1, object: 0, first: true }, Version.DRAFT_22);

	expect(Array.from(encoded)).toEqual([
		0x3c,
		1, // group id
		0, // object id
		0, // publisher priority
		0x02, // properties length
		0x10, // TIMESTAMP
		0x00, // 0
		0x00, // payload length, and nothing follows
	]);
});

// A track that declared no timescale opted out of timestamps, so the properties field is
// omitted rather than carrying a value in units the peer was never told. This is what a
// subscriber that sent INCLUDE_PROPERTIES=0 gets.
test("FetchFrame: an unstamped object omits the properties field", async () => {
	const frame = new FetchFrame({ payload: new Uint8Array([0x01]) });
	const encoded = await encodeFetchFrameVersioned(frame, { group: 0, object: 0, first: false }, Version.DRAFT_22);

	// Flags 0 (subgroup zero, no fields present), then the payload.
	expect(Array.from(encoded)).toEqual([0x00, 0x01, 0x01]);
});

// The first object still carries its absolute ids and priority; only the properties go.
test("FetchFrame: an unstamped first object keeps its ids", async () => {
	const frame = new FetchFrame({ payload: new Uint8Array([0x01]) });
	const encoded = await encodeFetchFrameVersioned(frame, { group: 4, object: 2, first: true }, Version.DRAFT_22);

	expect(Array.from(encoded)).toEqual([
		0x1c, // GROUP_ID | OBJECT_ID | PRIORITY, without PROPERTIES
		4,
		2,
		0,
		0x01,
		0x01,
	]);
});

/** Frame a control message body with its 16-bit Length, as every draft does. */
function framed(body: number[]): Uint8Array {
	return new Uint8Array([body.length >> 8, body.length & 0xff, ...body]);
}

/** Request ID 1, Track Namespace ("live"), Track Name ("video"): the head of a SUBSCRIBE or draft-22 FETCH. */
const TRACK_HEAD = [0x01, 0x01, 0x04, ...new TextEncoder().encode("live"), 0x05, ...new TextEncoder().encode("video")];

// Every parameter draft-22 lets a SUBSCRIBE carry that we don't act on still decodes, with
// what we have to refuse recorded rather than failing the stream.
test("Subscribe draft-22: decodes every legal parameter", async () => {
	const body = framed([
		...TRACK_HEAD,
		0x06, // Number of Parameters
		...[0x03, 0x03, 0x03, 0x00, 0xaa], // AUTHORIZATION TOKEN
		...[0x00, 0x03, 0x03, 0x00, 0xbb], // a second one, which the draft allows
		...[0x0d, 0x00], // FORWARD (0x10) = 0
		...[0x15, 0x03, 0x00, 0x00, 0x01], // SUBGROUP_FILTER (0x25)
		...[0x0d, 0x07], // NEW_GROUP_REQUEST (0x32)
		...[0x03, 0x00], // INCLUDE_PROPERTIES (0x35) = 0, a uint8
	]);

	const msg = await decodeVersioned(body, Subscribe.Subscribe.decode, Version.DRAFT_22);
	expect(msg.forward).toBe(false);
	expect(msg.rangeFilters).toBe(true);
	expect(msg.propertiesWanted).toBe(false);
});

// An omitted SUBSCRIBER_PRIORITY means 128, not the unset 127, so a relay forwards it unchanged.
test("Subscribe: an omitted SUBSCRIBER_PRIORITY is 128", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22]) {
		const msg = await decodeVersioned(framed([...TRACK_HEAD, 0x00]), Subscribe.Subscribe.decode, version);
		expect(msg.subscriberPriority).toBe(128);
	}
});

// INCLUDE_PROPERTIES is a uint8: one raw byte with no Length.
test("Subscribe v22: INCLUDE_PROPERTIES is a uint8", async () => {
	const msg = new Subscribe.Subscribe({
		requestId: 1n,
		trackNamespace: Path.from("live"),
		trackName: "video",
		subscriberPriority: 128,
		propertiesWanted: false,
	});
	const encoded = await encodeVersioned(msg, Version.DRAFT_22);
	expect(Array.from(encoded.slice(-2))).toEqual([0x13, 0x00]);
	expect(Array.from(encoded.slice(-3, -2))).not.toEqual([0x01]);
});

// FORWARD=0 decodes on every draft, so the request can be refused rather than failing.
test("Subscribe: FORWARD=0 decodes", async () => {
	for (const version of [Version.DRAFT_22] as const) {
		const msg = new Subscribe.Subscribe({
			requestId: 1n,
			trackNamespace: Path.from("live"),
			trackName: "video",
			subscriberPriority: 128,
			forward: false,
		});
		const decoded = await decodeVersioned(await encodeVersioned(msg, version), Subscribe.Subscribe.decode, version);
		expect(decoded.forward).toBe(false);
	}
});

// A draft-22 FETCH names the track up front and carries its range in LOCATION_FILTER.
// Groups 128 through 130: the group takes two bytes, so a Length (4) and the Location
// Filter Type (0x03) differ, and reading one as the other misframes every parameter after it.
test("Fetch draft-22: decodes every legal parameter", async () => {
	const body = framed([
		...TRACK_HEAD,
		0x07, // Number of Parameters
		...[0x03, 0x03, 0x03, 0x00, 0xaa], // AUTHORIZATION TOKEN
		...[0x07, 0x64], // FILL_TIMEOUT (0x0A)
		...[0x16, 0x40], // SUBSCRIBER_PRIORITY (0x20)
		...[0x01, 0x03, 0x80, 0x80, 0x00, 0x02], // LOCATION_FILTER (0x21)
		...[0x01, 0x01], // GROUP_ORDER (0x22)
		...[0x04, 0x02, 0x00, 0x05], // OBJECTID_FILTER (0x26)
		...[0x0f, 0x00], // INCLUDE_PROPERTIES (0x35)
	]);

	const msg = await decodeVersioned(body, Fetch.decode, Version.DRAFT_22);
	expect(msg.requestId).toBe(1n);
	expect(msg.fillTimeout).toBe(Milli(100));
});

// Draft-22's LOCATION_FILTER is a Location Filter Type with no Length, so Next Object is the
// single byte 0x05 and the parameter after it starts right behind it.
test("Subscribe draft-22: Next Object is 0x21 0x05", async () => {
	const body = framed([
		...TRACK_HEAD,
		0x04, // Number of Parameters
		...[0x10, 0x01], // FORWARD (0x10) = 1
		...[0x10, 0x40], // SUBSCRIBER_PRIORITY (0x20) = 64
		...[0x01, 0x05], // LOCATION_FILTER (0x21): Next Object
		...[0x01, 0x02], // GROUP_ORDER (0x22) = Descending
	]);

	const msg = await decodeVersioned(body, Subscribe.Subscribe.decode, Version.DRAFT_22);
	expect(msg.filter).toEqual({ kind: "nextObject" });
	expect(msg.subscriberPriority).toBe(64);
	expect(await encodeVersioned(msg, Version.DRAFT_22)).toEqual(body);
});

// TRACK_STATUS is identical to SUBSCRIBE on every draft, fields and parameters included.
test("TrackStatusRequest: carries SUBSCRIBE fields", async () => {
	const cases: [IetfVersion, number[]][] = [
		// AUTHORIZATION TOKEN and INCLUDE_PROPERTIES.
		[Version.DRAFT_22, [0x02, 0x03, 0x03, 0x03, 0x00, 0xaa, 0x32, 0x00]],
	];
	for (const [version, rest] of cases) {
		const msg = await decodeVersioned(framed([...TRACK_HEAD, ...rest]), Track.TrackStatusRequest.decode, version);
		expect(msg.trackName).toBe("video");
		expect(msg.propertiesWanted).toBe(version !== Version.DRAFT_22);
	}
});

// TRACK_PROPERTY_FILTER is legal only on SUBSCRIBE_TRACKS and its updates, so on a
// SUBSCRIBE or FETCH it stays a protocol violation rather than a per-request refusal.
test("TRACK_PROPERTY_FILTER is rejected on SUBSCRIBE and FETCH", async () => {
	// One parameter: TRACK_PROPERTY_FILTER (0x29), SetID 0, property type 2, from 0.
	const params = [0x01, 0x29, 0x03, 0x00, 0x02, 0x00];
	const body = framed([...TRACK_HEAD, ...params]);
	await expect(decodeVersioned(body, Subscribe.Subscribe.decode, Version.DRAFT_22)).rejects.toThrow();
	await expect(decodeVersioned(body, Fetch.decode, Version.DRAFT_22)).rejects.toThrow();
});

for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22]) {
	for (const kind of [0x20n, 0x21n, 0x9dn, 0x11cn]) {
		test(`SETUP ${version}: repeated unknown option ${kind}`, async () => {
			const { stream, written } = createTestWritableStream();
			const writer = new Writer(stream, version);
			await writer.u62(kind);
			for (let i = 0; i < 2; i++) {
				if (i > 0) await writer.u62(0n);
				if (kind % 2n === 0n) await writer.u62(1n);
				else {
					await writer.u53(1);
					await writer.u8(1);
				}
			}
			await writer.close();
			await writer.closed;
			await SetupOptions.decode(new Reader(undefined, concatChunks(written), version));
		});
	}
}

for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22]) {
	test(`SETUP ${version}: known options cannot repeat`, async () => {
		for (const bytes of [new Uint8Array([4, 1, 0, 2]), new Uint8Array([7, 1, 97, 0, 1, 98])]) {
			await expect(SetupOptions.decode(new Reader(undefined, bytes, version))).rejects.toThrow(/duplicate/);
		}
	});
	test(`SETUP ${version}: repeated unknown options still require complete values`, async () => {
		const bytes = new Uint8Array([0x21, 1, 97, 0, 2, 98]);
		await expect(SetupOptions.decode(new Reader(undefined, bytes, version))).rejects.toThrow();
	});
	test(`GROUP_ORDER ${version}: only ascending and descending are legal`, async () => {
		for (const value of [0, 3, 255]) {
			const bytes = new Uint8Array([1, 0x22, value]);
			await expect(Parameters.decode(new Reader(undefined, bytes, version), version)).rejects.toThrow(
				/group order/,
			);
		}
		for (const value of [1, 2]) {
			const bytes = new Uint8Array([1, 0x22, value]);
			const params = await Parameters.decode(new Reader(undefined, bytes, version), version);
			expect(params.groupOrder).toBe(value);
		}
	});
}

// Caller-selected SUBSCRIBE group order.
test.each([["v22", Version.DRAFT_22]] as const)(
	"Subscribe %s: group order defaults to descending",
	async (_name, version) => {
		const msg = new Subscribe.Subscribe({
			requestId: 1n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 128,
		});
		expect(msg.groupOrder).toBe("descending");

		const encoded = await encodeVersioned(msg, version);
		const decoded = await decodeVersioned(encoded, Subscribe.Subscribe.decode, version);
		expect(decoded.groupOrder).toBe("descending");
	},
);

test.each([["v22", Version.DRAFT_22]] as const)(
	"Subscribe %s: a caller can request ascending group order",
	async (_name, version) => {
		const msg = new Subscribe.Subscribe({
			requestId: 1n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 128,
			groupOrder: "ascending",
		});

		const encoded = await encodeVersioned(msg, version);

		// Differential, not a byte search: `toContain(1)` would pass on requestId 1n
		// alone. Encode the same message as descending and require that exactly one
		// byte moved, from 0x02 to 0x01.
		const descending = await encodeVersioned(
			new Subscribe.Subscribe({
				requestId: 1n,
				trackNamespace: Path.from("test"),
				trackName: "video",
				subscriberPriority: 128,
				groupOrder: "descending",
			}),
			version,
		);
		expect(encoded.length).toBe(descending.length);
		const differing = [...encoded].flatMap((byte, i) => (byte === descending[i] ? [] : [i]));
		expect(differing).toHaveLength(1);
		expect(encoded[differing[0]]).toBe(Subscribe.GroupOrder.Ascending);
		expect(descending[differing[0]]).toBe(Subscribe.GroupOrder.Descending);

		const decoded = await decodeVersioned(encoded, Subscribe.Subscribe.decode, version);
		expect(decoded.groupOrder).toBe("ascending");
	},
);
