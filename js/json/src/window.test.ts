import { describe, expect, test } from "bun:test";
import * as Moq from "@moq/net";
import * as Window from "./window.ts";

interface Rec {
	n: number;
}

function pair(compression = false): [Window.Producer<Rec>, Window.Consumer<Rec>] {
	const track = new Moq.Track.Producer("test");
	const consumer = new Window.Consumer<Rec>(track.subscribe(), { compression });
	return [new Window.Producer<Rec>(track, { compression }), consumer];
}

/** Drain every update the consumer can produce until the track ends. */
async function drain(consumer: Window.Consumer<Rec>): Promise<Window.Update<Rec>[]> {
	const out: Window.Update<Rec>[] = [];
	for (;;) {
		const update = await consumer.next();
		if (!update) return out;
		out.push(update);
	}
}

/** The records a consumer holds after applying every update. */
function replay(updates: Window.Update<Rec>[]): [number, Rec][] {
	let held: [number, Rec][] = [];
	for (const update of updates) {
		if (update.type === "append") held.push([update.position, update.value]);
		else held = held.filter(([position]) => position >= update.offset);
	}
	return held;
}

describe("window", () => {
	test("appends carry positions", async () => {
		const [producer, consumer] = pair();
		for (let n = 0; n < 3; n++) expect(producer.append({ n })).toBe(n);
		producer.finish();

		expect(await drain(consumer)).toEqual([
			{ type: "append", position: 0, value: { n: 0 } },
			{ type: "append", position: 1, value: { n: 1 } },
			{ type: "append", position: 2, value: { n: 2 } },
		]);
	});

	test("a trim retracts from the head", async () => {
		const [producer, consumer] = pair();
		for (let n = 0; n < 4; n++) producer.append({ n });
		producer.trim(2);
		expect(producer.offset).toBe(2);
		expect(producer.length).toBe(2);
		producer.finish();

		const updates = await drain(consumer);
		expect(updates.at(-1)).toEqual({ type: "trim", offset: 2 });
		expect(replay(updates)).toEqual([
			[2, { n: 2 }],
			[3, { n: 3 }],
		]);
	});

	test("a trim past the window throws", () => {
		const [producer] = pair();
		producer.append({ n: 0 });
		expect(() => producer.trim(2)).toThrow();
		// The rejected trim left the window untouched, and zero is a no-op.
		expect(producer.length).toBe(1);
		producer.trim(0);
	});

	test("a non-integer trim count is rejected", () => {
		// NaN fails every comparison, so a range check alone lets it through: it would splice
		// nothing, poison the offset, and serialize as `null`. A fraction splices nothing either
		// and emits a count both consumers reject.
		const [producer] = pair();
		for (let n = 0; n < 3; n++) producer.append({ n });

		for (const bad of [0.5, Number.NaN, Number.POSITIVE_INFINITY, -1]) {
			expect(() => producer.trim(bad)).toThrow();
		}
		expect(producer.length).toBe(3);
		expect(producer.offset).toBe(0);
	});

	test("heavy trimming rolls a group and still converges on the live window", async () => {
		const track = new Moq.Track.Producer("test");
		const producer = new Window.Producer<Rec>(track);

		producer.append({ n: 0 });
		producer.append({ n: 1 });
		for (let n = 2; n < 12; n++) {
			producer.append({ n });
			producer.trim(1);
		}
		producer.finish();

		// A late joiner skips to the newest buffered group rather than replaying every cached one.
		// Parity with the Rust consumer.
		const consumer = new Window.Consumer<Rec>(track.subscribe());
		const updates = await drain(consumer);
		const positions = updates.filter((u) => u.type === "append").map((u) => u.position);

		// It starts inside the newest group's snapshot, not back at the beginning of the log. The
		// exact position depends on where the last roll fell, so assert the property, not the value.
		expect(positions[0]).toBeGreaterThan(0);
		expect(positions).toEqual([...new Set(positions)]);
		expect(replay(updates)).toEqual([
			[10, { n: 10 }],
			[11, { n: 11 }],
		]);
	});

	test("mutating an appended record does not change what a later snapshot emits", async () => {
		// The window keeps records by value, not by reference. Otherwise a caller mutating an object
		// it already appended would make a later group's snapshot emit different JSON for the same
		// position, so a consumer that saw the first group and one that joined later would disagree
		// about a record they both name.
		const track = new Moq.Track.Producer("test");
		const producer = new Window.Producer<Rec>(track);

		const record = { n: 0 };
		producer.append(record);
		record.n = 999; // after publishing, before any roll

		// Roll via the per-group frame cap rather than by trimming, so position 0 is still in the
		// window and gets re-serialized into the new group's snapshot.
		for (let n = 1; n < 300; n++) producer.append({ n });
		producer.finish();

		const held = replay(await drain(new Window.Consumer<Rec>(track.subscribe())));
		const first = held.find(([position]) => position === 0);
		expect(first).toBeDefined();
		expect(first?.[1]).toEqual({ n: 0 });
	});

	test("a failed publish leaves the window untouched", () => {
		// `finish` only closes the track, so a later append is expressible. It must fail without
		// moving the window: `offset`, `length`, and the next position all read from it.
		const [producer] = pair();
		producer.append({ n: 0 });
		producer.append({ n: 1 });
		producer.finish();

		expect(() => producer.append({ n: 2 })).toThrow();
		expect(producer.length).toBe(2);
		expect(producer.offset).toBe(0);

		expect(() => producer.trim(1)).toThrow();
		expect(producer.length).toBe(2);
		expect(producer.offset).toBe(0);
	});

	test("a snapshot spanning past the exact-integer limit is rejected", async () => {
		// The offset alone is not enough: past 2^53-1 two positions round together, so the next
		// append would repeat one the consumer already yielded.
		const track = new Moq.Track.Producer("test");
		const consumer = new Window.Consumer<Rec>(track.subscribe());

		const group = track.appendGroup();
		const snapshot = { offset: Number.MAX_SAFE_INTEGER - 1, values: [{ n: 0 }, { n: 1 }, { n: 2 }] };
		group.writeFrame({
			payload: new TextEncoder().encode(JSON.stringify(snapshot)),
			timestamp: Moq.Time.Timestamp.now(),
		});
		group.close();
		track.close();

		await expect(drain(consumer)).rejects.toThrow("maximum position");
	});

	test("a group roll never repeats a record", async () => {
		// Whatever group a consumer lands on, positions are what make the switch lossless: a record
		// repeated by a later snapshot is skipped rather than yielded twice. (The strictly-live
		// interleaving is covered on the Rust side, which has a synchronous poll API to drive it.)
		const [producer, consumer] = pair();
		for (let n = 0; n < 2; n++) producer.append({ n });
		for (let n = 2; n < 12; n++) {
			producer.append({ n });
			producer.trim(1);
		}
		producer.finish();

		const positions = (await drain(consumer)).filter((u) => u.type === "append").map((u) => u.position);
		expect(positions).toEqual([...new Set(positions)]);
		expect([...positions].sort((a, b) => a - b)).toEqual(positions);
	});

	test("compressed roundtrip", async () => {
		const [producer, consumer] = pair(true);
		for (let n = 0; n < 20; n++) producer.append({ n });
		producer.trim(15);
		producer.finish();

		const held = replay(await drain(consumer));
		expect(held.length).toBe(5);
		expect(held[0]).toEqual([15, { n: 15 }]);
	});

	test("a null record keeps its position", async () => {
		// `null` is a valid JSON record. Treating it as an absent `append` would drop the record and
		// stall the tail, shifting every later position. Parity with the Rust window.
		const track = new Moq.Track.Producer("test");
		const consumer = new Window.Consumer<Rec | null>(track.subscribe());
		const producer = new Window.Producer<Rec | null>(track);

		producer.append({ n: 0 });
		producer.append(null);
		producer.append({ n: 2 });
		producer.finish();

		const out: Window.Update<Rec | null>[] = [];
		for (;;) {
			const update = await consumer.next();
			if (!update) break;
			out.push(update);
		}
		expect(out).toEqual([
			{ type: "append", position: 0, value: { n: 0 } },
			{ type: "append", position: 1, value: null },
			{ type: "append", position: 2, value: { n: 2 } },
		]);
	});

	test("a malformed trim is rejected", async () => {
		// A non-numeric trim would coerce to 0 and silently no-op, leaving the head desynced from
		// the publisher's; Rust errors on it too.
		const track = new Moq.Track.Producer("test");
		const consumer = new Window.Consumer<Rec>(track.subscribe());

		const group = track.appendGroup();
		for (const frame of [{ offset: 0, values: [{ n: 0 }] }, { trim: null }]) {
			group.writeFrame({
				payload: new TextEncoder().encode(JSON.stringify(frame)),
				timestamp: Moq.Time.Timestamp.now(),
			});
		}
		group.close();
		track.close();

		// Buffered frames drain in one pass, so the malformed trim surfaces on the first read.
		await expect(drain(consumer)).rejects.toThrow("invalid trim");
	});

	test("a frame carrying both operations is rejected", async () => {
		// Applying both would append and retract in an order the format never defines. Parity with
		// the Rust window; a frame carrying neither stays a no-op (see the next test).
		const track = new Moq.Track.Producer("test");
		const consumer = new Window.Consumer<Rec>(track.subscribe());

		const group = track.appendGroup();
		for (const frame of [
			{ offset: 0, values: [{ n: 0 }] },
			{ append: { n: 1 }, trim: 1 },
		]) {
			group.writeFrame({
				payload: new TextEncoder().encode(JSON.stringify(frame)),
				timestamp: Moq.Time.Timestamp.now(),
			});
		}
		group.close();
		track.close();

		await expect(drain(consumer)).rejects.toThrow("both an append and a trim");
	});

	test("a snapshot with an unusable offset is rejected", async () => {
		// Every position derives from the offset, and NaN comparisons are all false, so a bad offset
		// would keep emitting appends whose positions no reader can use rather than failing.
		for (const offset of [undefined, "4", Number.NaN, -1]) {
			const track = new Moq.Track.Producer("test");
			const consumer = new Window.Consumer<Rec>(track.subscribe());

			const group = track.appendGroup();
			group.writeFrame({
				payload: new TextEncoder().encode(JSON.stringify({ offset, values: [{ n: 0 }] })),
				timestamp: Moq.Time.Timestamp.now(),
			});
			group.close();
			track.close();

			await expect(drain(consumer)).rejects.toThrow("invalid offset");
		}
	});

	test("a snapshot missing values is rejected", async () => {
		// `values` is required. Treating an absent one as an empty window would report a head jump
		// and continue from fabricated state, where the Rust decoder fails outright.
		const track = new Moq.Track.Producer("test");
		const consumer = new Window.Consumer<Rec>(track.subscribe());

		const group = track.appendGroup();
		group.writeFrame({
			payload: new TextEncoder().encode(JSON.stringify({ offset: 4 })),
			timestamp: Moq.Time.Timestamp.now(),
		});
		group.close();
		track.close();

		await expect(drain(consumer)).rejects.toThrow("must be an array");
	});

	test("an append past the exact-integer limit is rejected", async () => {
		// The snapshot bound alone is not enough: a snapshot can land the tail exactly at the limit
		// and appends walk it past, where positions round together. Rust rejects the same frame.
		const track = new Moq.Track.Producer("test");
		const consumer = new Window.Consumer<Rec>(track.subscribe());

		const group = track.appendGroup();
		for (const frame of [{ offset: Number.MAX_SAFE_INTEGER - 1, values: [{ n: 0 }] }, { append: { n: 1 } }]) {
			group.writeFrame({
				payload: new TextEncoder().encode(JSON.stringify(frame)),
				timestamp: Moq.Time.Timestamp.now(),
			});
		}
		group.close();
		track.close();

		await expect(drain(consumer)).rejects.toThrow("reaches the maximum");
	});

	test("an unknown operation is ignored", async () => {
		// A frame carrying neither key is a forward-compatible extension, not an error. Written by
		// hand, since the producer only emits the operations it knows.
		const track = new Moq.Track.Producer("test");
		const consumer = new Window.Consumer<Rec>(track.subscribe());

		const group = track.appendGroup();
		for (const frame of [{ offset: 0, values: [{ n: 0 }] }, { rewind: 1 }, { append: { n: 1 } }]) {
			group.writeFrame({
				payload: new TextEncoder().encode(JSON.stringify(frame)),
				timestamp: Moq.Time.Timestamp.now(),
			});
		}
		group.close();
		track.close();

		expect(await drain(consumer)).toEqual([
			{ type: "append", position: 0, value: { n: 0 } },
			{ type: "append", position: 1, value: { n: 1 } },
		]);
	});

	test("a group with no snapshot is rejected", async () => {
		// A group that ends cleanly having carried nothing is malformed: every group opens with a
		// snapshot. Reporting it as a clean end of track would lose the window the publisher
		// advertised. Rust rejects the same group.
		const track = new Moq.Track.Producer("test");
		const consumer = new Window.Consumer<Rec>(track.subscribe());

		track.appendGroup().close();
		track.close();

		await expect(drain(consumer)).rejects.toThrow("no snapshot");
	});

	test("a hand-written trim is applied", async () => {
		// The wire shape the Rust producer emits, decoded independently of our own producer.
		const track = new Moq.Track.Producer("test");
		const consumer = new Window.Consumer<Rec>(track.subscribe());

		const group = track.appendGroup();
		for (const frame of [{ offset: 4, values: [{ n: 4 }, { n: 5 }] }, { append: { n: 6 } }, { trim: 2 }]) {
			group.writeFrame({
				payload: new TextEncoder().encode(JSON.stringify(frame)),
				timestamp: Moq.Time.Timestamp.now(),
			});
		}
		group.close();
		track.close();

		const updates = await drain(consumer);
		// Joining at offset 4 reports no trim for records it never held; the later trim is real.
		expect(updates).toEqual([
			{ type: "append", position: 4, value: { n: 4 } },
			{ type: "append", position: 5, value: { n: 5 } },
			{ type: "append", position: 6, value: { n: 6 } },
			{ type: "trim", offset: 6 },
		]);
		expect(replay(updates)).toEqual([[6, { n: 6 }]]);
	});
});
