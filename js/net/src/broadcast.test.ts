import { expect, setSystemTime, test } from "bun:test";
import { Broadcast } from "./broadcast.ts";
import { CacheFull, MAX_GROUP_FRAMES } from "./group.ts";
import { Timestamp } from "./time.ts";
import { TrackProducer } from "./track.ts";

test("subscribe serves a statically inserted track without a request", async () => {
	const broadcast = new Broadcast();

	const track1 = new TrackProducer("track1").accept();
	broadcast.insertTrack(track1);
	track1.appendGroup().close();

	// The track already exists, so subscribe resolves immediately (no requested()).
	const sub1 = broadcast.track("track1").subscribe();
	expect((await sub1.nextGroup())?.sequence).toBe(0);

	// No on-demand request was emitted for it.
	expect(broadcast.state.requested.peek().length).toBe(0);

	// A second static track behaves the same.
	const track2 = new TrackProducer("track2").accept();
	broadcast.insertTrack(track2);

	const sub2 = broadcast.track("track2").subscribe();
	track2.appendGroup().close();
	expect((await sub2.nextGroup())?.sequence).toBe(0);
});

test("two subscribers to one inserted track each get a full copy", async () => {
	const broadcast = new Broadcast();
	const producer = broadcast.createTrack("video");

	const a = broadcast.track("video").subscribe();
	const b = broadcast.track("video").subscribe();

	producer.writeString("hello");
	producer.writeString("world");

	// Neither subscriber steals from the other: both see every frame in order.
	expect(await a.readString()).toBe("hello");
	expect(await b.readString()).toBe("hello");
	expect(await a.readString()).toBe("world");
	expect(await b.readString()).toBe("world");
});

test("a late subscriber replays the cached window", async () => {
	const broadcast = new Broadcast();
	const producer = broadcast.createTrack("video");

	// Written before anyone subscribes; retained in the cache for replay.
	producer.writeString("early");

	const late = broadcast.track("video").subscribe();
	expect(await late.readString()).toBe("early");

	producer.writeString("later");
	expect(await late.readString()).toBe("later");
});

test("a read throws CacheFull on a gap, then resyncs to the next group", async () => {
	const broadcast = new Broadcast();
	const producer = broadcast.createTrack("video");
	const sub = broadcast.track("video").subscribe();

	// Group 0 overflows its frame cap without being read, evicting the front: a gap.
	const g0 = producer.appendGroup();
	for (let i = 0; i < MAX_GROUP_FRAMES + 10; i++)
		g0.writeFrame({ data: new Uint8Array([i & 0xff]), timestamp: Timestamp.now() });
	g0.close();

	// Group 1 is clean.
	const g1 = producer.appendGroup();
	g1.writeFrame({ data: new TextEncoder().encode("ok"), timestamp: Timestamp.now() });
	g1.close();

	// The reader hits the gap in group 0 (error, not a silent skip), then the next
	// read resyncs from group 1.
	expect(sub.readFrame()).rejects.toBeInstanceOf(CacheFull);
	expect(await sub.readString()).toBe("ok");
});

test("a stalled consumer does not pin evicted groups", () => {
	try {
		setSystemTime(new Date(10_000));

		const broadcast = new Broadcast();
		const producer = broadcast.createTrack("video", { cache: 1000 });

		// A subscriber that never reads. Its sink must not grow without bound.
		const stalled = broadcast.track("video").subscribe();

		producer.writeString("old");
		expect(stalled.state.groups.peek().length).toBe(1);

		// Advance past the cache window and write again to trigger a prune. The old
		// (closed, aged-out) group is dropped from the stalled sink, not retained.
		setSystemTime(new Date(12_000));
		producer.writeString("fresh");

		const groups = stalled.state.groups.peek();
		expect(groups.length).toBe(1);
		expect(groups[0].sequence).toBe(1);
	} finally {
		setSystemTime();
	}
});

test("createTrack commits info up front", async () => {
	const broadcast = new Broadcast();

	const producer = broadcast.createTrack("video", { cache: 2000, priority: 3 });
	expect(producer.name).toBe("video");

	const info = await broadcast.track("video").info();
	expect(info.cache).toBe(2000);
	expect(info.priority).toBe(3);
});

test("insertTrack rejects a duplicate live name", () => {
	const broadcast = new Broadcast();
	broadcast.createTrack("dup");
	expect(() => broadcast.insertTrack(new TrackProducer("dup").accept())).toThrow();
});

test("a closed track is evicted and re-subscribing falls through to a request", async () => {
	const broadcast = new Broadcast();

	const track1 = broadcast.createTrack("track1");
	track1.close();

	// The stale entry is gone, so subscribe creates an on-demand request instead.
	const pending = broadcast.track("track1").subscribe();
	expect(pending).toBeDefined();
	expect(broadcast.state.requested.peek().length).toBe(1);

	const request = await broadcast.requested();
	expect(request?.name).toBe("track1");
});

test("removeTrack drops the static entry", () => {
	const broadcast = new Broadcast();
	broadcast.createTrack("track1");
	expect(broadcast.state.tracks.has("track1")).toBe(true);

	broadcast.removeTrack("track1");
	expect(broadcast.state.tracks.has("track1")).toBe(false);
});
