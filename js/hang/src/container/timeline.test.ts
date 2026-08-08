import { expect, test } from "bun:test";
import * as Json from "@moq/json";
import type { Time } from "@moq/net";
import { Track } from "@moq/net";
import { MOQ_EPOCH_UNIX_MILLIS, u53 } from "../catalog";
import { Producer, type Record } from "./timeline.ts";

const us = (ms: number): Time.Micro => (ms * 1000) as Time.Micro;

/** A timeline whose published records are decoded back out of its MoQ track. */
function capture(props?: ConstructorParameters<typeof Producer>[1]): {
	timeline: Producer;
	records: () => Promise<Record[]>;
} {
	const track = new Track.Producer("timeline.z");
	const consumer = new Json.Stream.Consumer<Record>(track.subscribe(), { compression: true });
	const timeline = new Producer(track, props);

	const records = async () => {
		const out: Record[] = [];
		for (;;) {
			const record = await consumer.next();
			if (!record) return out;
			out.push(record);
		}
	};
	return { timeline, records };
}

// The coarsest track paces: video GOPs are longer than durationMin, so segments are GOPs.
test("the coarsest track paces the segments", async () => {
	const { timeline, records } = capture();
	const video = timeline.track("video0");
	const audio = timeline.track("audio0");

	// Video keyframes every 2s, audio groups every 500ms, minimum 1s.
	video.record(0, us(0));
	for (const [seq, ms] of [
		[0, 0],
		[1, 500],
		[2, 1000],
		[3, 1500],
	] as const) {
		audio.record(seq, us(ms));
	}
	video.record(1, us(2000));
	for (const [seq, ms] of [
		[4, 2000],
		[5, 2500],
		[6, 3000],
		[7, 3500],
	] as const) {
		audio.record(seq, us(ms));
	}
	video.record(2, us(4000));
	audio.record(8, us(4000));
	video.close();
	audio.close();
	timeline.finish();

	// Audio's own candidate (1s) loses to video's (2s), so no segment splits a GOP.
	expect(await records()).toEqual([
		{
			segment: 0,
			pts: 0,
			duration: 2000,
			tracks: { video0: [{ start: 0, end: 0 }], audio0: [{ start: 0, end: 3 }] },
		},
		{
			segment: 1,
			pts: 2000,
			duration: 2000,
			tracks: { video0: [{ start: 1, end: 1 }], audio0: [{ start: 4, end: 7 }] },
		},
		{
			segment: 2,
			pts: 4000,
			duration: 0,
			tracks: { video0: [{ start: 2, end: 2 }], audio0: [{ start: 8, end: 8 }] },
		},
	]);
});

// A real-time encoder's GOP can dwarf the minimum. Nothing is violated: there is nowhere else
// a segment could start and stay decodable, so the segment is simply long.
test("a GOP longer than the minimum is one segment", async () => {
	const { timeline, records } = capture();
	const video = timeline.track("video0");

	video.record(0, us(0));
	video.record(1, us(30_000));
	video.end(us(60_000));
	video.close();
	timeline.finish();

	expect(await records()).toEqual([
		{ segment: 0, pts: 0, duration: 30_000, tracks: { video0: [{ start: 0, end: 0 }] } },
		{ segment: 1, pts: 30_000, duration: 30_000, tracks: { video0: [{ start: 1, end: 1 }] } },
	]);
});

// Groups shorter than the minimum pack into one segment rather than each becoming one.
test("short groups pack up to the minimum", async () => {
	const { timeline, records } = capture({ durationMin: 1500 });
	const audio = timeline.track("audio0");

	for (let seq = 0; seq < 8; seq++) {
		audio.record(seq, us(seq * 500));
	}
	audio.close();
	timeline.finish();

	const out = await records();
	// The first group at or past 1500ms ends segment 0, so it holds groups 0..=2.
	expect(out[0]).toEqual({ segment: 0, pts: 0, duration: 1500, tracks: { audio0: [{ start: 0, end: 2 }] } });
	expect(out[1]).toEqual({ segment: 1, pts: 1500, duration: 1500, tracks: { audio0: [{ start: 3, end: 5 }] } });
});

// A track that hasn't reached the minimum yet holds the segment open, so a record never omits
// a rendition that was about to contribute to it.
test("a segment waits for every track", async () => {
	const { timeline, records } = capture();
	const video = timeline.track("video0");
	const audio = timeline.track("audio0");

	video.record(0, us(0));
	audio.record(0, us(0));
	// Video alone would close segment 0 here, but audio hasn't crossed 2s.
	video.record(1, us(2000));
	audio.record(1, us(2000));
	video.close();
	audio.close();
	timeline.finish();

	const out = await records();
	expect(out[0]).toEqual({
		segment: 0,
		pts: 0,
		duration: 2000,
		tracks: { video0: [{ start: 0, end: 0 }], audio0: [{ start: 0, end: 0 }] },
	});
});

// An application that knows its own boundaries overrides the pacing.
test("explicit cuts override the pacing", async () => {
	const { timeline, records } = capture();
	const video = timeline.track("video0");

	// Keyframes every second, cut every three: the segments follow the cuts, not the GOPs.
	timeline.cut(us(3000));
	timeline.cut(us(6000));
	for (let seq = 0; seq < 7; seq++) {
		video.record(seq, us(seq * 1000));
	}
	video.close();
	timeline.finish();

	const out = await records();
	expect(out[0]).toEqual({ segment: 0, pts: 0, duration: 3000, tracks: { video0: [{ start: 0, end: 2 }] } });
	expect(out[1]).toEqual({ segment: 1, pts: 3000, duration: 3000, tracks: { video0: [{ start: 3, end: 5 }] } });
});

// Every rendition of one import declares the same boundaries, so a cut that would make a
// segment shorter than the minimum is dropped rather than producing a stray segment.
test("a cut below the minimum is ignored", async () => {
	const { timeline, records } = capture();
	const video = timeline.track("video0");

	video.record(0, us(0));
	timeline.cut(us(2000));
	// A sibling rendition's duplicate, and a boundary too close to be a segment.
	timeline.cut(us(2000));
	timeline.cut(us(2500));
	timeline.cut(us(4000));
	video.record(1, us(2000));
	video.record(2, us(4000));
	video.close();
	timeline.finish();

	const out = await records();
	expect(out).toHaveLength(3);
	expect(out[0]).toEqual({ segment: 0, pts: 0, duration: 2000, tracks: { video0: [{ start: 0, end: 0 }] } });
	expect(out[1]).toEqual({ segment: 1, pts: 2000, duration: 2000, tracks: { video0: [{ start: 1, end: 1 }] } });
});

// The declared maximum is a contract with consumers (an HLS EXT-X-TARGETDURATION), so a
// segment that breaks it fails the timeline rather than publishing a record that contradicts
// the catalog.
test("exceeding the declared maximum fails the timeline", async () => {
	const { timeline, records } = capture({ durationMin: 1000, durationMax: 3000 });
	const video = timeline.track("video0");

	video.record(0, us(0));
	video.record(1, us(2000));
	// A 4s GOP against a declared 3s maximum.
	video.record(2, us(6000));
	video.close();

	expect(() => timeline.finish()).toThrow(/over the declared maximum/);

	// The record that was still true published; the one that would have contradicted the
	// catalog did not.
	expect(await records()).toEqual([
		{ segment: 0, pts: 0, duration: 2000, tracks: { video0: [{ start: 0, end: 0 }] } },
	]);
});

test("an undeclared maximum is omitted from the catalog", () => {
	expect(capture().timeline.section().durationMax).toBeUndefined();
	expect(capture({ durationMax: 2500 }).timeline.section().durationMax).toBe(u53(2500));
});

// The last group of a broadcast has no successor to bound it, so without a reported end the
// final segment's duration collapses to zero (an HLS EXTINF:0).
test("the final segment runs to the reported end", async () => {
	const { timeline, records } = capture();
	const video = timeline.track("video0");

	video.record(0, us(0));
	video.record(1, us(2000));
	// A closing container producer reports where its content stops.
	video.end(us(4000));
	video.close();
	timeline.finish();

	expect(await records()).toEqual([
		{ segment: 0, pts: 0, duration: 2000, tracks: { video0: [{ start: 0, end: 0 }] } },
		{ segment: 1, pts: 2000, duration: 2000, tracks: { video0: [{ start: 1, end: 1 }] } },
	]);
});

// A record is immutable and judged against the tracks enrolled when it flushes, so a producer
// that enrolls its tracks one batch at a time holds flushing back until they are all in.
test("a reservation defers flushing until every track enrolls", async () => {
	const { timeline, records } = capture();
	const release = timeline.reserve();

	// The primary rendition runs a whole batch of segments through before its sibling exists.
	const first = timeline.track("video0");
	for (const [seq, ms] of [
		[0, 0],
		[1, 2000],
		[2, 4000],
	] as const) {
		first.record(seq, us(ms));
	}

	const second = timeline.track("video1");
	for (const [seq, ms] of [
		[0, 0],
		[1, 2000],
		[2, 4000],
	] as const) {
		second.record(seq, us(ms));
	}

	release();
	first.close();
	second.close();
	timeline.finish();

	// Both renditions are indexed from segment 0. Without the hold, segments 0 and 1 would have
	// flushed knowing only video0, and video1's first two groups would have folded into
	// segment 2.
	const out = await records();
	expect(out[0]).toEqual({
		segment: 0,
		pts: 0,
		duration: 2000,
		tracks: { video0: [{ start: 0, end: 0 }], video1: [{ start: 0, end: 0 }] },
	});
	expect(out[1]).toEqual({
		segment: 1,
		pts: 2000,
		duration: 2000,
		tracks: { video0: [{ start: 1, end: 1 }], video1: [{ start: 1, end: 1 }] },
	});
});

// A track that races ahead of the others still lands in the segment its content falls in.
test("groups before the first boundary join the first segment", async () => {
	const { timeline, records } = capture();
	const video = timeline.track("video0");
	const audio = timeline.track("audio0");

	audio.record(0, us(0));
	video.record(0, us(30));
	for (const [seq, ms] of [
		[1, 500],
		[2, 1000],
		[3, 1500],
		[4, 2000],
	] as const) {
		audio.record(seq, us(ms));
	}
	video.record(1, us(2030));
	audio.record(5, us(2500));
	video.close();
	audio.close();
	timeline.finish();

	const out = await records();
	expect(out[0]).toEqual({
		// The segment starts where the earliest content does, not at video's first keyframe.
		segment: 0,
		pts: 0,
		duration: 2030,
		tracks: { video0: [{ start: 0, end: 0 }], audio0: [{ start: 0, end: 4 }] },
	});
});

test("sequence gaps split ranges", async () => {
	const { timeline, records } = capture();
	const video = timeline.track("video0");
	const audio = timeline.track("audio0");

	// Elemental-style gap: audio groups 2..=4 never existed inside segment 0.
	video.record(0, us(0));
	audio.record(0, us(0));
	audio.record(1, us(300));
	audio.record(5, us(1500));
	video.record(1, us(2000));
	audio.record(6, us(2100));
	video.close();
	audio.close();
	timeline.finish();

	const out = await records();
	expect(out[0]).toEqual({
		segment: 0,
		pts: 0,
		duration: 2000,
		tracks: {
			video0: [{ start: 0, end: 0 }],
			audio0: [
				{ start: 0, end: 1 },
				{ start: 5, end: 5 },
			],
		},
	});
});

// A track with nothing to contribute is a whole-segment gap: the record simply omits it (an
// HLS exporter renders EXT-X-GAP). Only a closed track produces one, since a live track that
// has merely gone quiet still gets to say where the boundary is.
test("a closed track leaves a whole segment gap", async () => {
	const { timeline, records } = capture();
	const video = timeline.track("video0");
	const audio = timeline.track("audio0");

	video.record(0, us(0));
	audio.record(0, us(0));
	audio.close();
	video.record(1, us(2000));
	video.record(2, us(4000));
	video.close();
	timeline.finish();

	const out = await records();
	expect(out[1]).toEqual({ segment: 1, pts: 2000, duration: 2000, tracks: { video0: [{ start: 1, end: 1 }] } });
});

test("a non-keyframe range start is flagged", async () => {
	const { timeline, records } = capture();
	const video = timeline.track("video0");

	video.record(0, us(0));
	// A mid-stream join: the group doesn't open on an IDR.
	video.record(1, us(2000), false);
	video.record(2, us(4000));
	video.close();
	timeline.finish();

	const out = await records();
	expect(out[1]?.tracks?.video0[0].keyframe).toBe(false);
});

test("the wall-clock anchor is advertised from the config", () => {
	expect(capture().timeline.section().wall).toBeUndefined();

	// The wire counts from the moq epoch, so pts 0 at exactly the epoch advertises 0.
	const epoch = new Date(MOQ_EPOCH_UNIX_MILLIS);
	expect(capture({ wall: epoch }).timeline.section().wall).toBe(u53(0));
	expect(capture({ wall: new Date(MOQ_EPOCH_UNIX_MILLIS + 1000) }).timeline.section().wall).toBe(u53(1000));

	// A time before the epoch isn't representable, so it clamps rather than going negative.
	expect(capture({ wall: new Date(0) }).timeline.section().wall).toBe(u53(0));
});

// Reservations nest like the catalog's, so the first batch to finish doesn't publish records
// the others are still filling in.
test("reservations nest", async () => {
	const { timeline, records } = capture();

	const outer = timeline.reserve();
	const inner = timeline.reserve();

	const first = timeline.track("video0");
	for (const [seq, ms] of [
		[0, 0],
		[1, 2000],
		[2, 4000],
	] as const) {
		first.record(seq, us(ms));
	}

	// If reservations didn't nest, releasing this one would publish segment 0 without video1.
	inner();

	const second = timeline.track("video1");
	for (const [seq, ms] of [
		[0, 0],
		[1, 2000],
		[2, 4000],
	] as const) {
		second.record(seq, us(ms));
	}

	outer();
	first.close();
	second.close();
	timeline.finish();

	const out = await records();
	expect(out[0]).toEqual({
		segment: 0,
		pts: 0,
		duration: 2000,
		tracks: { video0: [{ start: 0, end: 0 }], video1: [{ start: 0, end: 0 }] },
	});
	expect(out[1]).toEqual({
		segment: 1,
		pts: 2000,
		duration: 2000,
		tracks: { video0: [{ start: 1, end: 1 }], video1: [{ start: 1, end: 1 }] },
	});
});

// A cut registered before the media, landing exactly on the first reported group (what the fMP4
// importer does: it cuts at the keyframe fragment's timestamp, then records that group). The
// spent cut must not block the ones behind it, and durationMin pacing must not race ahead of them.
test("a cut on the first group does not poison later cuts", async () => {
	const { timeline, records } = capture();
	const video = timeline.track("video0");

	// Source segments every 3s, keyframes every 1s: 3s segments, not the 1s the durationMin
	// pacing would produce on its own.
	timeline.cut(us(0));
	video.record(0, us(0));
	for (let seq = 1; seq < 10; seq++) {
		if (seq % 3 === 0) timeline.cut(us(seq * 1000));
		video.record(seq, us(seq * 1000));
	}
	video.close();
	timeline.finish();

	const out = await records();
	expect(out[0]).toEqual({ segment: 0, pts: 0, duration: 3000, tracks: { video0: [{ start: 0, end: 2 }] } });
	expect(out[1]).toEqual({ segment: 1, pts: 3000, duration: 3000, tracks: { video0: [{ start: 3, end: 5 }] } });
});

// A catalog publishes a group only when the renditions change, so it can go quiet for the rest
// of the broadcast. Enrolled as a pacing track it would stall the timeline for good; passive, it
// rides along in whichever segment is open.
test("a passive track neither paces nor gates", async () => {
	const { timeline, records } = capture();
	const video = timeline.track("video0");
	const catalog = timeline.passive("catalog.json");

	catalog.record(0, us(0));
	video.record(0, us(0));
	video.record(1, us(2000));
	video.record(2, us(4000));
	video.end(us(6000));
	video.close();
	timeline.finish();

	expect(await records()).toEqual([
		{
			segment: 0,
			pts: 0,
			duration: 2000,
			tracks: { "catalog.json": [{ start: 0, end: 0 }], video0: [{ start: 0, end: 0 }] },
		},
		{ segment: 1, pts: 2000, duration: 2000, tracks: { video0: [{ start: 1, end: 1 }] } },
		{ segment: 2, pts: 4000, duration: 2000, tracks: { video0: [{ start: 2, end: 2 }] } },
	]);
});

// Nothing waits for a passive track, so a group arriving after its segment already flushed is
// recorded in the next one. Placement is by arrival; the frames still carry their own timestamps.
test("a late passive group lands in the next segment", async () => {
	const { timeline, records } = capture();
	const video = timeline.track("video0");
	const catalog = timeline.passive("catalog.json");

	video.record(0, us(0));
	// Flushes segment 0, which the catalog has contributed nothing to.
	video.record(1, us(2000));

	// The update happened a second in, but only reaches the timeline now.
	catalog.record(0, us(1000));

	video.record(2, us(4000));
	video.end(us(6000));
	video.close();
	timeline.finish();

	const out = await records();
	expect(out[0]).toEqual({ segment: 0, pts: 0, duration: 2000, tracks: { video0: [{ start: 0, end: 0 }] } });
	expect(out[1]).toEqual({
		segment: 1,
		pts: 2000,
		duration: 2000,
		tracks: { "catalog.json": [{ start: 0, end: 0 }], video0: [{ start: 1, end: 1 }] },
	});
});
