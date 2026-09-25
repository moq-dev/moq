import { heapStats } from "bun:jsc";
import { expect, test } from "bun:test";
import { Container } from "@moq/hang";
import * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect } from "@moq/signals";
import { nextMedia, subscribeMedia } from "./media";
import { Sync } from "./sync";

// The player path a decoder drives, one frame per group like AAC audio: the container consumer
// reads each frame, the shared clock anchors on it, and presentation waits on the clock against
// the effect's teardown. Retention anywhere along it grows the heap with the frame count.
test("a long subscription through the player path keeps a flat heap", async () => {
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

	const heap = () => {
		Bun.gc(true);
		return heapStats().objectCount;
	};

	await play(200);
	const before = heap();
	await play(2000);
	expect(heap() - before).toBeLessThan(1000);

	consumer.close();
	effect.close();
	sync.close();
	broadcast.close();
});
