import { expect, spyOn, test } from "bun:test";
import { Group, Track } from "@moq/net";
import * as z from "@zod/mini";
import { Decoder } from "./decoder.ts";
import { Encoder } from "./encoder.ts";
import { Producer } from "./producer.ts";

type Rec = { n: number };

test("roundtrip in order", () => {
	const encoder = new Encoder<Rec>({});
	const decoder = new Decoder<Rec>({});
	for (let n = 0; n < 20; n += 1) {
		const record = encoder.encode({ n });
		expect(decoder.decode(record.payload)).toEqual({ n });
		record.commit();
	}
});

// A record carries no shared state, so a dropped one leaves a gap in the log rather than an
// undecodable stream, and the encoder keeps going.
test("an uncommitted record does not stop the encoder", () => {
	const encoder = new Encoder<Rec>({});
	encoder.encode({ n: 0 });

	const record = encoder.encode({ n: 1 });
	const decoder = new Decoder<Rec>({});
	expect(decoder.decode(record.payload)).toEqual({ n: 1 });
	record.commit();
});

// `JSON.stringify` yields undefined for a top-level undefined, function, or symbol. Framing that
// would write empty bytes and fail on the consumer rather than here.
test("a record JSON cannot represent is rejected", () => {
	const encoder = new Encoder<unknown>({});
	expect(() => encoder.encode(undefined)).toThrow("not representable as JSON");
	expect(() => encoder.encode(() => {})).toThrow("not representable as JSON");
});

// A record the encoder rejects must not have published a group first: a live consumer would advance
// into it and wait there even though nothing was ever appended.
test("a rejected record ends the track without opening a group", async () => {
	const track = new Track.Producer("test");
	const subscriber = track.subscribe().ordered();
	const producer = new Producer<unknown>({ track });

	expect(() => producer.append({ value: undefined })).toThrow("not representable as JSON");

	// Nothing was appended, so the log has no group for a consumer to enter and wait in.

	// And the log is missing a record, so the track ends rather than staying writable: a later
	// append would present that gap as a complete log. Matches the Rust producer.
	await expect(subscriber.nextGroup()).rejects.toThrow("not representable as JSON");
});

test("a failed write on the very first record still ends the track", async () => {
	// The first append opens group 0 and then fails its write, which is as terminal as any later
	// one: the group is live, so leaving the track writable would let a second group present the
	// missing record as a complete log, and leave a consumer waiting in the empty group 0.
	const track = new Track.Producer("test");
	const subscriber = track.subscribe().ordered();
	const producer = new Producer<string>({ track });

	// The budget check refuses anything the group would, so stub the write to stand in for any
	// rejection past it: `appendGroup` succeeds and `writeFrame` throws.
	const failure = new Error("write rejected");
	const write = spyOn(Group.Producer.prototype, "writeFrame").mockImplementation(() => {
		throw failure;
	});
	try {
		expect(() => producer.append({ value: "x" })).toThrow(failure);
	} finally {
		write.mockRestore();
	}

	// The consumer is handed the group that was already published, and reading it surfaces the
	// abort. Without ending the track that group stays open and empty, so this read hangs instead.
	const group = await subscriber.nextGroup();
	expect(group).toBeDefined();
	await expect(group?.readFrame()).rejects.toThrow(failure);
});

test("stream schema validates on both sides", () => {
	const schema = z.object({ n: z.number() });
	const encoder = new Encoder<Rec>({ schema });
	const decoder = new Decoder<Rec>({ schema });
	expect(() => encoder.encode({ n: "bad" } as unknown as Rec)).toThrow();
	expect(decoder.decode(encoder.encode({ n: 1 }).payload)).toEqual({ n: 1 });
	expect(() => decoder.decode(new TextEncoder().encode('{"n":"bad"}'))).toThrow();
});
