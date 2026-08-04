import { expect, test } from "bun:test";
import { Track } from "@moq/net";
import { Decoder } from "./decoder.ts";
import { Encoder } from "./encoder.ts";
import { Producer } from "./producer.ts";

type Rec = { n: number };

function roundtrip(compression: boolean, values: Rec[]): Rec[] {
	const encoder = new Encoder<Rec>({ compression });
	const decoder = new Decoder<Rec>({ compression });
	return values.map((value) => {
		const record = encoder.encode(value);
		const decoded = decoder.decode(record.payload);
		record.commit();
		return decoded;
	});
}

test("plaintext roundtrip in order", () => {
	const values = Array.from({ length: 5 }, (_, n) => ({ n }));
	expect(roundtrip(false, values)).toEqual(values);
});

test("compressed roundtrip in order", () => {
	const values = Array.from({ length: 20 }, (_, n) => ({ n }));
	expect(roundtrip(true, values)).toEqual(values);
});

test("the shared window shrinks repetitive records", () => {
	const encoder = new Encoder<{ group: number; pts: number }>({ compression: true });
	const sizes = Array.from({ length: 8 }, (_, n) => {
		const record = encoder.encode({ group: n, pts: n * 2_000 });
		record.commit();
		return record.payload.length;
	});

	const raw = new TextEncoder().encode(JSON.stringify({ group: 7, pts: 14_000 })).length;
	expect(sizes.at(-1)).toBeLessThan(raw / 2);
});

// A caller that rolls a group has to restart both windows, or the new group's frames decode against
// context the decoder on the other side never received.
test("reset starts a cold window on both sides", () => {
	const encoder = new Encoder<Rec>({ compression: true });
	const decoder = new Decoder<Rec>({ compression: true });

	for (let n = 0; n < 4; n += 1) {
		const record = encoder.encode({ n });
		expect(decoder.decode(record.payload)).toEqual({ n });
		record.commit();
	}

	encoder.reset();
	decoder.reset();

	const record = encoder.encode({ n: 99 });
	expect(decoder.decode(record.payload)).toEqual({ n: 99 });
	record.commit();
});

// A compressed record that never reached the wire leaves the window ahead of the consumer, and a log
// has no keyframe to resynchronize on. Continuing would emit frames nothing can decode, so the
// encoder refuses until the caller rolls a new group and resets.
test("an uncommitted compressed record stops the encoder", () => {
	const encoder = new Encoder<Rec>({ compression: true });
	encoder.encode({ n: 0 }).commit();

	// This one fails to write, so the caller never commits it.
	encoder.encode({ n: 1 });

	expect(() => encoder.encode({ n: 2 })).toThrow("compression desynchronized");

	// Rolling a new group gives the consumer a cold window too, so the reset clears it.
	encoder.reset();
	const record = encoder.encode({ n: 2 });
	const decoder = new Decoder<Rec>({ compression: true });
	expect(decoder.decode(record.payload)).toEqual({ n: 2 });
	record.commit();
});

// Without compression a record carries no shared state, so a dropped one leaves a gap in the log
// rather than an undecodable stream, and the encoder keeps going.
test("an uncommitted plaintext record does not stop the encoder", () => {
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
test("a rejected record does not open a group", async () => {
	const track = new Track.Producer("test");
	const subscriber = track.subscribe();
	const producer = new Producer<unknown>(track);

	expect(() => producer.append(undefined)).toThrow("not representable as JSON");

	// Nothing was appended, so the log has no group for a consumer to enter and wait in.
	producer.finish();
	expect(await subscriber.nextGroup()).toBeUndefined();
});

// The same stale-commit hazard the snapshot encoder has: acknowledging a superseded record must not
// clear the flag belonging to the newer one, or a desync goes undetected.
test("a stale commit does not clear a newer pending record", () => {
	const encoder = new Encoder<Rec>({ compression: true });
	const first = encoder.encode({ n: 0 });
	first.commit();

	// This one fails to write, so it stays outstanding. Committing the old handle a second time must
	// not acknowledge it on its behalf, or the desync goes undetected.
	encoder.encode({ n: 1 });
	first.commit();

	expect(() => encoder.encode({ n: 2 })).toThrow("compression desynchronized");
});

// A record committed after the encoder has been reset must not acknowledge whatever is outstanding
// now: the newer record belongs to a different group and window.
test("a commit from before a reset does not acknowledge a newer record", () => {
	const encoder = new Encoder<Rec>({ compression: true });
	const stale = encoder.encode({ n: 0 });

	// The caller rolled a new group and reset, then encoded into it.
	encoder.reset();
	encoder.encode({ n: 1 });

	// The old record's write finally settles, far too late to speak for the new one.
	stale.commit();

	expect(() => encoder.encode({ n: 2 })).toThrow("compression desynchronized");
});
