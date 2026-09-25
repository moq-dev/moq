import { expect, spyOn, test } from "bun:test";
import { Container } from "@moq/hang";
import * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { nextMedia, subscribeMedia } from "./media";
import { Sync } from "./sync";

// These count what is still attached rather than the heap: `Bun.gc` scans the stack conservatively,
// so a stale pointer can pin thousands of dead cells and fail a heap count under load.

// Reactions `run` attaches to promises still pending once it returns, which each hold until they
// settle. Recorded by hand: Bun's `mock.contexts` misses the engine's own calls from `Promise.race`.
async function pendingReactions(run: () => Promise<void>): Promise<number> {
	const reacted: Promise<unknown>[] = [];
	const then = Promise.prototype.then;
	const spy = spyOn(Promise.prototype, "then").mockImplementation(function (this: Promise<unknown>, ...args) {
		reacted.push(this);
		return then.apply(this, args);
	} as typeof then);
	try {
		await run();
	} finally {
		spy.mockRestore();
	}
	return reacted.filter((promise) => Bun.peek.status(promise) === "pending").length;
}

// Signal listeners `run` registers that neither fired nor were disposed by the time it returns.
async function pendingListeners(run: () => Promise<void>): Promise<number> {
	const listening = new Set<object>();
	const changed = Signal.prototype.changed;
	const spy = spyOn(Signal.prototype, "changed").mockImplementation(function (
		this: Signal<unknown>,
		fn?: (value: unknown) => void,
	) {
		if (!fn) return (changed as () => Promise<unknown>).call(this);
		const token = {};
		listening.add(token);
		const dispose = changed.call(this, (value) => {
			listening.delete(token);
			fn(value);
		});
		return () => {
			listening.delete(token);
			dispose();
		};
	} as typeof changed);
	try {
		await run();
	} finally {
		spy.mockRestore();
	}
	return listening.size;
}

// Frames sleep on the clock once each, so a sleep that keeps anything on a clock that never changes
// piles it up for the life of the player.
test("waits on a stable clock leave nothing behind", async () => {
	const sync = new Sync({ delay: Time.Milli(10) });
	// Let the delay effect run before anchoring.
	await new Promise((resolve) => setTimeout(resolve, 0));
	sync.received(Time.Milli.now());

	const wait = async () => {
		for (let round = 0; round < 10; round++) {
			const now = Time.Milli.now();
			await Promise.all(Array.from({ length: 100 }, () => sync.wait(now)));
		}
	};
	expect(await pendingReactions(wait)).toBe(0);
	expect(await pendingListeners(wait)).toBe(0);

	sync.close();
});

// The player path a decoder drives, one frame per group like AAC audio: the container consumer
// reads each frame, the shared clock anchors on it, and presentation waits on the clock against
// the effect's teardown. Retention anywhere along it grows with the frame count.
test("a long subscription through the player path leaves nothing behind", async () => {
	const broadcast = new Moq.Broadcast.Producer();
	// A tiny publisher window, so the track's own replay cache stays flat too.
	const track = broadcast.createTrack("audio", { maxAge: Time.Milli(1) });
	const format = new Container.Legacy.Format("audio");
	const producer = new Container.Legacy.Producer(track, format);

	const sync = new Sync({ delay: Time.Milli(10) });
	const effect = new Effect();
	const sub = subscribeMedia(effect, {
		broadcast: broadcast.consume(),
		track: "audio",
		priority: 0,
		maxAge: sync.out.maxAge,
	});
	if (!sub) throw new Error("no subscription");
	const consumer = new Container.Consumer(sub, { format, maxAge: sync.out.maxAge });

	// Presentations overlap, as a decoder's outputs do, so the clock's sleeps are shared.
	const presenting = new Set<Promise<unknown>>();
	const play = async (count: number) => {
		for (let i = 0; i < count; i++) {
			producer.encode(new Uint8Array([i & 0xff]), Math.round(Time.Micro.now()) as Time.Micro, true);
			// A new group closes the previous one with a duration marker, which reads as no frame.
			let next = await nextMedia(consumer);
			while (next && !next.frame) next = await nextMedia(consumer);
			if (!next?.frame) throw new Error("the track ended");

			const timestamp = Time.Milli.fromMicro(next.frame.timestamp);
			sync.received(timestamp);
			const presented = effect.race(sync.wait(timestamp));
			presenting.add(presented);
			void presented.finally(() => presenting.delete(presented));
		}
		await Promise.all(presenting);
	};

	// What stays is bounded by the track's window of open groups, not the frame count.
	let reactions = 0;
	const listeners = await pendingListeners(async () => {
		reactions = await pendingReactions(() => play(2000));
	});
	expect(reactions).toBeLessThan(100);
	expect(listeners).toBeLessThan(100);

	consumer.close();
	effect.close();
	sync.close();
	broadcast.close();
});
