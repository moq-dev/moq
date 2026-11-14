// The rules a lite-07 copy's `RangeFills` settles its fills by. Mirrors the tests in the Rust
// `lite/subscriber/fills.rs`.
import { expect, spyOn, test } from "bun:test";
import { yieldNow } from "@moq/clock";
import { Signal } from "@moq/signals";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import { StreamCode, StreamError } from "../error.ts";
import { hooks, type Run } from "../internal.ts";
import { Tail } from "../tail.ts";
import type { Fill, Producer } from "../track.ts";
import { dropped, passing, RangeFills, type RecoveryHandle, type Refused } from "./fills.ts";

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
	const opened: {
		groups: Run;
		priority: number;
		finish(refused: Refused[]): void;
		refuse(refused: Refused): void;
		cancelled: boolean;
		reading: boolean;
		readingChanged: Signal<number>;
	}[] = [];
	const recover = (groups: Run, priority: number): RecoveryHandle => {
		let finish!: (refused: Refused[]) => void;
		const done = new Promise<Refused[]>((resolve) => {
			finish = resolve;
		});
		const soFar: Refused[] = [];
		const changed = new Signal(0);
		const refuse = (refused: Refused) => {
			soFar.push(refused);
			changed.update((revision) => revision + 1);
		};
		const entry = {
			groups: { ...groups },
			priority,
			finish,
			refuse,
			cancelled: false,
			reading: false,
			readingChanged: new Signal(0),
		};
		opened.push(entry);
		return {
			done,
			takeRefused: () => soFar.splice(0),
			changed,
			readingChanged: entry.readingChanged,
			reading: () => entry.reading,
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
	// With the fill's priority. The recovered run leaves the subscription, though the rest of the
	// fill's run stays asked: left in, any later update would reopen its stale drop there, and the
	// publisher would serve it on both.
	expect(opened[0]?.priority).toBe(peer.fill.priority);
	expect(fills.ask.groups).toEqual([
		{ start: 0n, end: 1n },
		{ start: 3n, end: 4n },
	]);

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

// The subscription holds its FIN while the fills are not empty, so a recovery ending after the
// ask emptied still reports the change.
test("range fills: the last recovery finishing reports the fills empty", async () => {
	const peer = await missing();
	const { opened, recover } = recoveries();
	const fills = new RangeFills(peer.track, recover);
	const tail = new Tail();
	fills.push(peer.fill);
	fills.poll(tail, true);
	fills.refuse(1n, 3n, new StreamError(StreamCode.Old));
	fills.poll(tail, true);
	const recovery = opened[0];
	if (!recovery) throw new Error("no recovery");

	// Every group arrives while a stream of the recovery is still read, which keeps it.
	recovery.reading = true;
	for (let sequence = peer.fill.wanted(); sequence !== undefined; sequence = peer.fill.wanted())
		peer.fill.accept(sequence).finish();
	expect(fills.poll(tail, true)).toBe(true);
	expect(fills.ask.groups).toEqual([]);
	expect(fills.isEmpty()).toBe(false);

	// The ask stays empty as the recovery ends.
	recovery.reading = false;
	recovery.finish([]);
	await yieldNow();
	expect(fills.poll(tail, true)).toBe(true);
	expect(fills.isEmpty()).toBe(true);
	expect(fills.poll(tail, true)).toBe(false);

	fills.close();
	peer.close();
});

// A recovery that ends before the fill reaches its groups still answers them once it does, with
// the publisher's own verdict: a group it never had is recorded, so no reader asks for it again.
test("range fills: a recovery's refusals stand until the fill reaches them", async () => {
	const peer = await missing();
	const { opened, recover } = recoveries();
	const fills = new RangeFills(peer.track, recover);
	const tail = new Tail();
	fills.push(peer.fill);
	fills.poll(tail, true);
	tail.account(1n, 3n);
	fills.refuse(1n, 3n, new StreamError(StreamCode.Old));
	fills.poll(tail, true);
	opened[0]?.finish([{ start: 1n, end: 3n, refusal: dropped(new StreamError(StreamCode.NotFound)) }]);
	await yieldNow();
	fills.poll(tail, true);
	// The fill still waits on group 0.
	expect(peer.fill.wanted()).toBe(0n);

	peer.fill.accept(0n).finish();
	fills.poll(tail, true);
	expect(peer.fill.wanted()).toBe(3n);
	// Recorded as lost: a later reader is told so rather than waiting on them.
	const later = peer.track.subscribe({ ranges: [{ start: { absolute: 1n } }], delay: { max: null } });
	const next = hooks.tryRecvGroup(later, true);
	expect(next.kind === "drop" ? [next.start, next.end] : next.kind).toEqual([1n, 3n]);

	later.close();
	fills.close();
	peer.close();
});

// The serve loop waits on the fills once per tail change, so a wait that loses keeps nothing
// registered: a stalled fill under a busy live edge would otherwise grow without bound.
test("range fills: a lost wait releases what it listened to", async () => {
	const peer = await missing();
	const { recover } = recoveries();
	const fills = new RangeFills(peer.track, recover);
	const tail = new Tail();
	fills.push(peer.fill);
	fills.poll(tail, true);
	fills.refuse(1n, 3n, new StreamError(StreamCode.Old));
	fills.poll(tail, true);

	// Count the change listeners every signal still holds.
	const original = Signal.prototype.changed;
	const awaited = original as (this: Signal<unknown>) => Promise<unknown>;
	const listened = original as (this: Signal<unknown>, fn: (value: unknown) => void) => () => void;
	let live = 0;
	const changed = spyOn(Signal.prototype, "changed").mockImplementation(function (
		this: Signal<unknown>,
		fn?: (value: unknown) => void,
	) {
		live++;
		if (!fn) return awaited.call(this).finally(() => live--);
		let held = true;
		const release = () => {
			if (held) live--;
			held = false;
		};
		const dispose = listened.call(this, (value: unknown) => {
			release();
			fn(value);
		});
		return () => {
			release();
			dispose();
		};
	} as typeof original);
	try {
		// Woken by the tail each time, as live groups arriving wake the serve loop.
		for (let sequence = 10n; sequence < 110n; sequence++) {
			const wait = fills.changed(tail);
			tail.account(sequence, sequence + 1n);
			await wait;
		}
		expect(live).toBe(0);
	} finally {
		changed.mockRestore();
	}

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

// A recovery opened on the first poll that holds its fill asks with that fill's priority, not the
// empty ask's lowest.
test("range fills: a recovery the first poll opens asks with the fill's priority", async () => {
	const peer = await missing();
	const { opened, recover } = recoveries();
	const fills = new RangeFills(peer.track, recover);
	const tail = new Tail();
	// The subscription accounted for groups 1 and 2 before the fill arrived.
	tail.account(1n, 3n);
	fills.push(peer.fill);
	fills.poll(tail, true);
	expect(opened.map((entry) => entry.groups)).toEqual([{ start: 1n, end: 3n }]);
	expect(opened[0]?.priority).toBe(peer.fill.priority);

	fills.close();
	peer.close();
});

// A refusal the source did not name lost answers only the fills owing it then, so a later fill
// for the same groups asks again rather than being refused by it.
test("range fills: a passing refusal ends with the fills it answered", async () => {
	const peer = await missing();
	const { opened, recover } = recoveries();
	const fills = new RangeFills(peer.track, recover);
	const tail = new Tail();
	fills.push(peer.fill);
	fills.poll(tail, true);
	fills.refuse(1n, 3n, new StreamError(StreamCode.Old));
	fills.poll(tail, true);
	opened[0]?.finish([{ start: 1n, end: 3n, refusal: passing(new StreamError(StreamCode.Cancel)) }]);
	await yieldNow();
	peer.fill.accept(0n).finish();
	fills.poll(tail, true);
	expect(peer.fill.wanted()).toBe(3n);
	peer.fill.close();
	fills.poll(tail, true);

	const requests = peer.track.requests();
	const later = peer.track.subscribe({
		ranges: [{ start: { absolute: 1n } }],
		delay: { max: null },
		order: "ascending",
	});
	void later
		.ordered()
		.nextGroup()
		.catch(() => undefined);
	const again = await requests.requestedFill();
	if (!again) throw new Error("no fill");
	fills.push(again);
	fills.poll(tail, true);
	expect(again.wanted()).toBe(1n);

	again.close();
	later.close();
	requests.close();
	fills.close();
	peer.close();
});

// A recovery still out answers the fills owing what it already refused, rather than holding them
// until its other streams finish.
test("range fills: a recovery's refusal answers before it is over", async () => {
	const peer = await missing();
	const { opened, recover } = recoveries();
	const fills = new RangeFills(peer.track, recover);
	const tail = new Tail();
	fills.push(peer.fill);
	fills.poll(tail, true);
	fills.refuse(1n, 3n, new StreamError(StreamCode.Old));
	fills.poll(tail, true);
	peer.fill.accept(0n).finish();
	fills.poll(tail, true);
	expect(peer.fill.wanted()).toBe(1n);

	// Group 1 is refused while group 2's stream is still out.
	const woken = fills.changed(tail);
	opened[0]?.refuse({ start: 1n, end: 2n, refusal: dropped(new StreamError(StreamCode.NotFound)) });
	await woken;
	fills.poll(tail, true);
	expect(peer.fill.wanted()).toBe(2n);

	fills.close();
	peer.close();
});

// A recovery still out hands each refusal over once: after the fills owing it end, a later fill for
// the same groups asks again rather than reading it back while the recovery reads another stream.
test("range fills: a recovery's refusal is taken once", async () => {
	const peer = await missing();
	const { opened, recover } = recoveries();
	const fills = new RangeFills(peer.track, recover);
	const tail = new Tail();
	fills.push(peer.fill);
	fills.poll(tail, true);
	fills.refuse(1n, 3n, new StreamError(StreamCode.Old));
	fills.poll(tail, true);
	const recovery = opened[0];
	if (!recovery) throw new Error("no recovery");
	// Still reading another of its streams, so it stays out with no fill owing it.
	recovery.reading = true;
	peer.fill.accept(0n).finish();
	recovery.refuse({ start: 1n, end: 2n, refusal: passing(new StreamError(StreamCode.Cancel)) });
	fills.poll(tail, true);
	expect(peer.fill.wanted()).toBe(2n);
	peer.fill.close();
	fills.poll(tail, true);
	expect(fills.recovering).toBe(true);

	const requests = peer.track.requests();
	const later = peer.track.subscribe({
		ranges: [{ start: { absolute: 1n } }],
		delay: { max: null },
		order: "ascending",
	});
	void later
		.ordered()
		.nextGroup()
		.catch(() => undefined);
	const again = await requests.requestedFill();
	if (!again) throw new Error("no fill");
	fills.push(again);
	fills.poll(tail, true);
	expect(again.wanted()).toBe(1n);
	// Asked again, rather than left to the recovery's other streams.
	expect(fills.ask.groups.some((run) => run.start <= 1n && 1n < run.end)).toBe(true);

	again.close();
	later.close();
	requests.close();
	fills.close();
	peer.close();
});

// A recovery ending leaves a later one's coverage of groups it released intact, so those groups
// stay off the copy's subscription while the later one asks for them.
test("range fills: an older recovery ending keeps a newer one's coverage", async () => {
	const peer = await missing();
	const { opened, recover } = recoveries();
	const fills = new RangeFills(peer.track, recover);
	const tail = new Tail();
	fills.push(peer.fill);
	fills.poll(tail, true);
	fills.refuse(1n, 3n, new StreamError(StreamCode.Old));
	fills.poll(tail, true);
	const older = opened[0];
	if (!older) throw new Error("no recovery");
	// It refuses group 1 and keeps reading group 2, past the fill.
	older.reading = true;
	peer.fill.accept(0n).finish();
	older.refuse({ start: 1n, end: 2n, refusal: passing(new StreamError(StreamCode.Cancel)) });
	fills.poll(tail, true);
	peer.fill.close();
	fills.poll(tail, true);

	// A later fill finds group 1 accounted for on the subscription, so a newer recovery asks for it.
	tail.account(1n, 2n);
	const requests = peer.track.requests();
	const later = peer.track.subscribe({
		ranges: [{ start: { absolute: 1n } }],
		delay: { max: null },
		order: "ascending",
	});
	void later
		.ordered()
		.nextGroup()
		.catch(() => undefined);
	const again = await requests.requestedFill();
	if (!again) throw new Error("no fill");
	fills.push(again);
	fills.poll(tail, true);
	expect(opened.map((entry) => entry.groups)).toEqual([
		{ start: 1n, end: 3n },
		{ start: 1n, end: 2n },
	]);

	// The older one ends first.
	older.reading = false;
	older.finish([]);
	await yieldNow();
	fills.poll(tail, true);
	expect(fills.ask.groups.some((run) => run.start <= 1n && 1n < run.end)).toBe(false);

	again.close();
	later.close();
	requests.close();
	fills.close();
	peer.close();
});

// The subscription repeating a stale drop, under a later update, of groups a recovery still asks
// for refuses nothing: the fill waits on the recovery's answer.
test("range fills: a stale drop repeated during its recovery waits on it", async () => {
	const peer = await missing();
	const { recover } = recoveries();
	const fills = new RangeFills(peer.track, recover);
	const tail = new Tail();
	fills.push(peer.fill);
	fills.poll(tail, true);
	fills.refuse(1n, 3n, new StreamError(StreamCode.Old));
	fills.poll(tail, true);
	fills.refuse(1n, 3n, new StreamError(StreamCode.Old));
	fills.poll(tail, true);
	peer.fill.accept(0n).finish();
	fills.poll(tail, true);
	expect(peer.fill.wanted()).toBe(1n);

	fills.close();
	peer.close();
});

// A recovery kept only for a stream still being read wakes the fills once that stream ends, so
// they cancel it rather than sleep on it.
test("range fills: a recovery's stream ending wakes the fills", async () => {
	const peer = await missing();
	const { opened, recover } = recoveries();
	const fills = new RangeFills(peer.track, recover);
	const tail = new Tail();
	fills.push(peer.fill);
	fills.poll(tail, true);
	fills.refuse(1n, 3n, new StreamError(StreamCode.Old));
	fills.poll(tail, true);
	const recovery = opened[0];
	if (!recovery) throw new Error("no recovery");
	recovery.reading = true;
	peer.fill.close();
	fills.poll(tail, true);
	expect(recovery.cancelled).toBe(false);

	const wait = fills.changed(tail).then(() => true);
	recovery.reading = false;
	recovery.readingChanged.update((revision) => revision + 1);
	const idle = (async () => {
		for (let i = 0; i < 10; i++) await yieldNow();
		return false;
	})();
	expect(await Promise.race([wait, idle])).toBe(true);
	fills.poll(tail, true);
	expect(recovery.cancelled).toBe(true);

	fills.close();
	peer.close();
});
