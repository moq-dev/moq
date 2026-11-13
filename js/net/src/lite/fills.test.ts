// The rules a lite-07 copy's `RangeFills` settles its fills by. Mirrors the tests in the Rust
// `lite/subscriber/fills.rs`.
import { expect, test } from "bun:test";
import { yieldNow } from "@moq/clock";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import { StreamCode, StreamError } from "../error.ts";
import type { Run } from "../internal.ts";
import { Tail } from "../tail.ts";
import type { Fill, Producer } from "../track.ts";
import { RangeFills, type RecoveryHandle, type Refused } from "./fills.ts";

const enc = new TextEncoder();

// A copy whose first group is 4, read from group 0 with no budget, and the fill for 0 to 4 its
// reader asks of it.
async function missing(): Promise<{ broadcast: BroadcastProducer; track: Producer; fill: Fill; close(): void }> {
	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video", { maxAge: null });
	const group = track.createGroup(4n);
	group.writeFrame({ payload: enc.encode("live 4") });
	group.finish();
	const requests = track.requests();
	const reader = track.subscribe({ ranges: [{ start: { absolute: 0n } }], delay: { max: null }, order: "ascending" });
	// Reading is what asks.
	void reader
		.ordered()
		.nextGroup()
		.catch(() => undefined);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	expect([fill.start, fill.end]).toEqual([0n, 4n]);
	return {
		broadcast,
		track,
		fill,
		close() {
			fill.close();
			reader.close();
			requests.close();
			broadcast.close();
		},
	};
}

// Recoveries a test answers by hand.
function recoveries() {
	const opened: { groups: Run; priority: number; finish(refused: Refused[]): void; cancelled: boolean }[] = [];
	const recover = (groups: Run, priority: number): RecoveryHandle => {
		let finish!: (refused: Refused[]) => void;
		const done = new Promise<Refused[]>((resolve) => {
			finish = resolve;
		});
		const entry = { groups: { ...groups }, priority, finish, cancelled: false };
		opened.push(entry);
		return {
			done,
			reading: () => false,
			cancel: () => {
				entry.cancelled = true;
				finish([]);
			},
		};
	};
	return { opened, recover };
}

test("range fills: a fill of more than one group joins the subscription's ranges", async () => {
	const peer = await missing();
	const { opened, recover } = recoveries();
	const fills = new RangeFills(peer.track, recover);
	fills.push(peer.fill);
	expect(fills.poll(new Tail(), true)).toBe(true);
	expect(fills.ask.groups).toEqual([{ start: 0n, end: 4n }]);
	// Asked through the subscription alone, and nothing changed since.
	expect(opened).toEqual([]);
	expect(fills.poll(new Tail(), true)).toBe(false);

	fills.close();
	expect(fills.ask.groups).toEqual([]);
	peer.close();
});

test("range fills: a stale drop of asked groups is recovered once, on a subscription of its own", async () => {
	const peer = await missing();
	const { opened, recover } = recoveries();
	const fills = new RangeFills(peer.track, recover);
	const tail = new Tail();
	fills.push(peer.fill);
	fills.poll(tail, true);

	fills.refuse(1n, 3n, new StreamError(StreamCode.Old));
	fills.poll(tail, true);
	expect(opened.map((entry) => entry.groups)).toEqual([{ start: 1n, end: 3n }]);
	// With the fill's priority. The run already asked stays asked whole while the fill owes any of
	// it, so the subscription does not change under the recovery.
	expect(opened[0]?.priority).toBe(peer.fill.priority);
	expect(fills.ask.groups).toEqual([{ start: 0n, end: 4n }]);

	// The recovery delivered nothing, and the next stale drop of the same groups stands.
	opened[0]?.finish([]);
	await yieldNow();
	fills.poll(tail, true);
	fills.refuse(1n, 3n, new StreamError(StreamCode.Old));
	fills.poll(tail, true);
	expect(opened.length).toBe(1);
	expect(peer.fill.wanted()).toBe(0n);
	peer.fill.accept(0n).finish();
	fills.poll(tail, true);
	// Refused for this fill, which moves on past them.
	expect(peer.fill.wanted()).toBe(3n);

	fills.close();
	peer.close();
});

test("range fills: a range start past the asked groups refuses only the fills owing them now", async () => {
	const peer = await missing();
	const { opened, recover } = recoveries();
	const fills = new RangeFills(peer.track, recover);
	const tail = new Tail();
	fills.push(peer.fill);
	fills.poll(tail, true);

	// The range for 0 to 4 started at 2.
	fills.skip({ start: 0n, end: 4n }, 0n, 2n);
	fills.poll(tail, true);
	expect(peer.fill.wanted()).toBe(2n);
	expect(opened).toEqual([]);

	// Nothing recorded: the track still holds no verdict on groups 0 and 1, so a later reader asks
	// for them again.
	const requests = peer.track.requests();
	const later = peer.track.subscribe({
		ranges: [{ start: { absolute: 0n } }],
		delay: { max: null },
		order: "ascending",
	});
	void later
		.ordered()
		.nextGroup()
		.catch(() => undefined);
	const again = await requests.requestedFill();
	expect(again?.wanted()).toBe(0n);

	again?.close();
	later.close();
	requests.close();
	fills.close();
	peer.close();
});
