import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import * as Path from "../path.ts";
import { Cursor, Reader, Writer } from "../stream.ts";
import { FetchOk } from "./fetch.ts";
import type * as Filter from "./filter.ts";
import { framed, unframed } from "./message.test-util.ts";
import * as Message from "./message.ts";
import { decodeFetchObject, EndOfRange, FetchEndOfRange, objectDeltaBias } from "./object.ts";
import { Publish } from "./publish.ts";
import { RequestError, RequestOk } from "./request.ts";
import { Setup } from "./setup.ts";
import { Subscribe, SubscribeUpdate } from "./subscribe.ts";
import { SubscribeNamespaceEntry } from "./subscribe_namespace.ts";
import { type IetfVersion, Version } from "./version.ts";

const OLD = [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22] as const;
const NEW = Version.DRAFT_23;

const encode = (msg: { encode(w: Writer, v: IetfVersion): Promise<void> }, version: IetfVersion) =>
	Writer.collect(version, (w) => msg.encode(w, version));
const reader = (bytes: ArrayLike<number>, version: IetfVersion) =>
	new Reader(undefined, Uint8Array.from(bytes), version);
const ascii = (text: string) => [...new TextEncoder().encode(text)];

/** Request ID 1, Track Namespace "live", Track Name "video". */
const TRACK_HEAD = [0x01, 0x01, 0x04, ...ascii("live"), 0x05, ...ascii("video")];

// Section 9: every control message's Message Length is a varint, SETUP's included.
test("the Message Length is a varint from draft-23", async () => {
	const body = (w: Writer) => w.write(new Uint8Array([0xaa, 0xaa, 0xaa]));
	for (const version of OLD) {
		expect(await Writer.collect(version, (w) => Message.encode(w, body))).toEqual(
			Uint8Array.of(0x00, 0x03, 0xaa, 0xaa, 0xaa),
		);
	}
	expect(await Writer.collect(NEW, (w) => Message.encode(w, body))).toEqual(Uint8Array.of(0x03, 0xaa, 0xaa, 0xaa));

	// A length past one varint byte still reads back whole.
	const long = new Uint8Array(300).fill(0x55);
	for (const version of [Version.DRAFT_22, NEW]) {
		const wire = await Writer.collect(version, (w) => Message.encode(w, (b) => b.write(long)));
		expect(await Message.decode(reader(wire, version), (r) => r.readAll())).toEqual(long);
	}

	const setup = await encode(new Setup(), NEW);
	expect(setup).toEqual(framed(unframed(setup, NEW), NEW));
	expect(setup[0]).toBe(setup.length - 1);
});

// We advertise no MAX_CONTROL_MESSAGE_SIZE, so the default 65535 bounds both directions.
test("a draft-23 Message Length is bounded at 65535", async () => {
	const big = new Uint8Array(Message.MAX_CONTROL_MESSAGE_SIZE + 1);
	await expect(Writer.collect(NEW, (w) => Message.encode(w, (b) => b.write(big)))).rejects.toThrow("too large");
	const length = await Writer.collect(NEW, (w) => w.u62(BigInt(Message.MAX_CONTROL_MESSAGE_SIZE + 1)));
	await expect(Message.decode(reader(length, NEW), (r) => r.readAll())).rejects.toThrow("too large");
});

// Section 9.19.18: PAUSED (0x11) replaces FORWARD (0x10) with the opposite sense.
test("PAUSED carries the Forward State at draft-23", async () => {
	for (const forward of [true, false]) {
		const msg = new Subscribe({
			requestId: 1n,
			trackNamespace: Path.from("live"),
			trackName: "video",
			subscriberPriority: 128,
			forward,
		});
		const wire = await encode(msg, NEW);
		expect(Array.from(unframed(wire, NEW))).toEqual([
			...TRACK_HEAD,
			0x03, // Number of Parameters
			...[0x11, forward ? 0 : 1], // PAUSED
			...[0x0f, 0x80], // SUBSCRIBER_PRIORITY (0x20) = 128
			...[0x02, 0x02], // GROUP_ORDER (0x22) = Descending
		]);
		expect((await Subscribe.decode(reader(wire, NEW), NEW)).forward).toBe(forward);

		const update = new SubscribeUpdate({ requestId: 10n });
		update.parameters.forward = forward;
		const bytes = await encode(update, NEW);
		expect(Array.from(unframed(bytes, NEW))).toEqual([0x0a, 0x01, 0x11, forward ? 0 : 1]);
		expect((await SubscribeUpdate.decode(reader(bytes, NEW), NEW)).parameters.forward).toBe(forward);
	}

	// An absent PAUSED means unpaused.
	expect((await Subscribe.decode(reader(framed([...TRACK_HEAD, 0x00], NEW), NEW), NEW)).forward).toBe(true);

	// Each draft refuses the other's parameter as unknown.
	await expect(Subscribe.decode(reader(framed([...TRACK_HEAD, 0x01, 0x10, 0x01], NEW), NEW), NEW)).rejects.toThrow(
		ProtocolViolation,
	);
	const draft22 = Version.DRAFT_22;
	await expect(
		Subscribe.decode(reader(framed([...TRACK_HEAD, 0x01, 0x11, 0x00], draft22), draft22), draft22),
	).rejects.toThrow(ProtocolViolation);
});

// Section 9.19.15: FILL_PARAMETERS drops its Length, so the parameter after it is found by
// reading the fill's own Parameters to the last one counted.
test("FILL_PARAMETERS is counted at draft-23", async () => {
	const fill: Filter.Fill = { filter: { kind: "relative", groups: 1n }, rangeFilters: false };
	const msg = new Subscribe({
		requestId: 1n,
		trackNamespace: Path.from("live"),
		trackName: "video",
		subscriberPriority: 128,
		fill,
	});
	const fillBytes = [0x23, 0x01, 0x21, 0x01, 0x01];
	// Count, FORWARD or PAUSED, SUBSCRIBER_PRIORITY = 128, GROUP_ORDER = Descending, then the fill.
	const head = (forward: number) => [
		...TRACK_HEAD,
		0x04,
		forward,
		forward === 0x10 ? 0x01 : 0x00,
		0x20 - forward,
		0x80,
		0x02,
		0x02,
	];
	const draft22 = Version.DRAFT_22;
	expect(Array.from(unframed(await encode(msg, draft22), draft22))).toEqual([
		...head(0x10),
		...[0x01, 0x04, 0x01, 0x21, 0x01, 0x01],
	]);
	expect(Array.from(unframed(await encode(msg, NEW), NEW))).toEqual([
		...head(0x11),
		...[0x01, 0x01, 0x21, 0x01, 0x01],
	]);

	const body = [
		...TRACK_HEAD,
		0x02, // Number of Parameters
		...fillBytes, // FILL_PARAMETERS: one Parameter, LOCATION_FILTER Relative Start 1
		...[0x0f, 0x07], // NEW_GROUP_REQUEST (0x32) = 7
	];
	const decoded = await Subscribe.decode(reader(framed(body, NEW), NEW), NEW);
	expect(decoded.fill?.filter).toEqual({ kind: "relative", groups: 1n });

	// The draft-22 spelling is not accepted in its place: its Length reads as the count.
	const prefixed = [...TRACK_HEAD, 0x01, 0x23, 0x04, 0x01, 0x21, 0x01, 0x01];
	await expect(Subscribe.decode(reader(framed(prefixed, NEW), NEW), NEW)).rejects.toThrow();
});

// Section 9.4.2: Error Details follow the Reason Phrase and are read past.
test("REQUEST_ERROR's Error Details are read and dropped at draft-23", async () => {
	const body = [
		0x10, // Error Code
		0x00, // Retry Interval
		0x02,
		...ascii("no"), // Reason Phrase
		...[0x01, 0x03, 0x00, 0x00, 0x00], // REDIRECT (0x01), three bytes
		...[0x01, 0x07], // 0x02 = 7
		...[0x00, 0x09], // and a second one, which the type allows
	];
	const msg = await RequestError.decode(reader(framed(body, NEW), NEW), NEW);
	expect([msg.errorCode, msg.reasonPhrase]).toEqual([0x10, "no"]);

	// A detail cut short is malformed, not skipped.
	await expect(RequestError.decode(reader(framed(body.slice(0, 7), NEW), NEW), NEW)).rejects.toThrow();

	// Before draft-23 nothing follows the Reason Phrase, so the details are trailing bytes.
	const draft22 = Version.DRAFT_22;
	await expect(RequestError.decode(reader(framed(body, draft22), draft22), draft22)).rejects.toThrow();

	// We send none, so our own is the same body on both drafts.
	const ours = new RequestError({ errorCode: 0x10, reasonPhrase: "no" });
	expect(unframed(await encode(ours, NEW), NEW)).toEqual(Uint8Array.from(body.slice(0, 5)));
});

// Section 9.15: NAMESPACE always ends with a parameter block, which defines no parameter of its own.
test("NAMESPACE carries an empty parameter block at draft-23", async () => {
	const entry = new SubscribeNamespaceEntry({ suffix: Path.from("a") });
	for (const version of OLD)
		expect(Array.from(unframed(await encode(entry, version), version))).toEqual([1, 1, 0x61]);
	const wire = await encode(entry, NEW);
	expect(Array.from(unframed(wire, NEW))).toEqual([1, 1, 0x61, 0]);
	expect((await SubscribeNamespaceEntry.decode(reader(wire, NEW), NEW)).suffix).toBe(Path.from("a"));

	// Without an extension, any parameter is unknown and cannot be skipped.
	await expect(
		SubscribeNamespaceEntry.decode(reader(framed([1, 1, 0x61, 1, 0x3e, 0], NEW), NEW), NEW),
	).rejects.toThrow(ProtocolViolation);
});

// Section 9.3: REQUEST_OK answers a FETCH, ending at LARGEST_OBJECT when it carries one.
test("FETCH is answered with REQUEST_OK at draft-23", async () => {
	for (const version of OLD) expect(FetchOk.idFor(version)).toBe(0x18);
	expect(FetchOk.idFor(NEW)).toBe(RequestOk.id);

	const largest = { group: 5n, object: 3n };
	const ok = new FetchOk({ endOfTrack: false, endLocation: largest, largest });
	expect(Array.from(unframed(await encode(ok, NEW), NEW))).toEqual([1, 0x09, 5, 3]);
	expect((await FetchOk.decode(reader(framed([1, 0x09, 5, 3], NEW), NEW), NEW)).largest).toEqual(largest);

	// An answer that ends at the requested end names no Largest Object.
	const bounded = new FetchOk({ endOfTrack: false, endLocation: largest });
	expect(Array.from(unframed(await encode(bounded, NEW), NEW))).toEqual([0]);
	expect((await FetchOk.decode(reader(framed([0], NEW), NEW), NEW)).largest).toBeUndefined();

	// EXPIRES is read and dropped; any parameter the draft does not list is refused.
	const decoded = await FetchOk.decode(reader(framed([2, 0x08, 5, 0x01, 7, 0], NEW), NEW), NEW);
	expect(decoded.largest).toEqual({ group: 7n, object: 0n });
	await expect(FetchOk.decode(reader(framed([1, 0x22, 1], NEW), NEW), NEW)).rejects.toThrow(ProtocolViolation);
});

// Section 11.4.1: End of Group, End of Track and End of Gone Range are draft-23 only.
test("fetch stream indicators are draft-23 only", async () => {
	const decode = (bytes: Uint8Array, version: IetfVersion) => decodeFetchObject(new Cursor(bytes, version));
	const gone = await Writer.collect(NEW, (w) =>
		new FetchEndOfRange({ reason: EndOfRange.Gone, group: 1n, object: 2n }).encode(w, NEW),
	);
	const endOfGroup = Uint8Array.of(0x80, 0x80);
	const endOfTrack = Uint8Array.of(0x80, 0x81);

	expect(decode(endOfGroup, NEW)).toEqual({ kind: "endOfGroup", track: false });
	expect(decode(endOfTrack, NEW)).toEqual({ kind: "endOfGroup", track: true });
	expect(decode(gone, NEW)).toEqual({ kind: "endOfRange", reason: EndOfRange.Gone, group: 1n, object: 2 });
	for (const version of OLD) {
		for (const bytes of [endOfGroup, endOfTrack, gone]) expect(() => decode(bytes, version)).toThrow();
		await expect(
			Writer.collect(version, (w) =>
				new FetchEndOfRange({ reason: EndOfRange.Gone, group: 1n, object: 2n }).encode(w, version),
			),
		).rejects.toThrow("draft-23");
	}
	// Another value from 128 up is unknown on every draft.
	for (const version of [Version.DRAFT_22, NEW]) expect(() => decode(Uint8Array.of(0x80, 0x82), version)).toThrow();

	// Within a group an Object ID Delta reaches two further at draft-23.
	for (const version of OLD) expect(objectDeltaBias(version)).toBe(0);
	expect(objectDeltaBias(NEW)).toBe(2);
});

// Section 9.8: PUBLISH may carry FILL_PARAMETERS and SUBSCRIBE_TRACKS_ID, read and dropped.
test("PUBLISH reads FILL_PARAMETERS and SUBSCRIBE_TRACKS_ID at draft-23", async () => {
	const body = (version: IetfVersion) =>
		framed(
			[
				...TRACK_HEAD,
				0x01, // track alias
				0x02, // Number of Parameters
				...[0x23, 0x01, 0x21, 0x05], // FILL_PARAMETERS: LOCATION_FILTER Next Object
				...[0x13, 0x04], // SUBSCRIBE_TRACKS_ID (0x36) = 4
			],
			version,
		);
	expect((await Publish.decode(reader(body(NEW), NEW), NEW)).trackAlias).toBe(1n);
	const draft22 = Version.DRAFT_22;
	await expect(Publish.decode(reader(body(draft22), draft22), draft22)).rejects.toThrow(ProtocolViolation);
});
