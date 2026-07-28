import { expect, test } from "bun:test";
import type { Time } from "@moq/net";
import { type Record, Segmenter } from "./timeline.ts";

const us = (ms: number): Time.Micro => (ms * 1000) as Time.Micro;

/** A segmenter whose flushed records are captured into the returned array. */
function capture(): { segmenter: Segmenter; records: Record[] } {
	const segmenter = new Segmenter();
	const records: Record[] = [];
	segmenter.attach((record) => records.push(record));
	return { segmenter, records };
}

test("auto-cut paces on video keyframes and waits for every track", () => {
	const { segmenter, records } = capture();
	const video = segmenter.track("video0", "video");
	const audio = segmenter.track("audio0", "audio");

	// Video keyframes every 2s (the default durationMax), audio groups every 500ms.
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
	expect(records).toHaveLength(0); // audio hasn't crossed the boundary yet
	audio.record(4, us(2000));

	// Segment 0 is complete on both tracks: self-contained, with explicit duration and
	// inclusive group ranges per track.
	expect(records).toEqual([
		{
			segment: 0,
			pts: 0,
			duration: 2000,
			tracks: { video0: [{ start: 0, end: 0 }], audio0: [{ start: 0, end: 3 }] },
		},
	]);

	video.close();
	audio.close();
	segmenter.finish();
	expect(records).toHaveLength(2);
	expect(records[1]).toEqual({
		segment: 1,
		pts: 2000,
		duration: 0,
		tracks: { video0: [{ start: 1, end: 1 }], audio0: [{ start: 4, end: 4 }] },
	});
});

test("explicit cuts disable auto-cut and pack multiple GOPs per segment", () => {
	const { segmenter, records } = capture();
	const video = segmenter.track("video0", "video");

	segmenter.cut(us(0));
	segmenter.cut(us(6000));
	for (const [seq, ms] of [
		[0, 0],
		[1, 2000],
		[2, 4000],
		[3, 6000],
	] as const) {
		video.record(seq, us(ms));
	}

	// Auto-cut (2s) must not fire between the explicit cuts: one 6s segment of three GOPs.
	expect(records).toEqual([{ segment: 0, pts: 0, duration: 6000, tracks: { video0: [{ start: 0, end: 2 }] } }]);
});

test("audio-only paces itself at durationMax", () => {
	const { segmenter, records } = capture();
	const audio = segmenter.track("audio0", "audio");

	for (const [seq, ms] of [
		[0, 0],
		[1, 500],
		[2, 1000],
		[3, 1500],
		[4, 2000],
	] as const) {
		audio.record(seq, us(ms));
	}

	expect(records).toEqual([{ segment: 0, pts: 0, duration: 2000, tracks: { audio0: [{ start: 0, end: 3 }] } }]);
});

test("sequence gaps split ranges", () => {
	const { segmenter, records } = capture();
	const video = segmenter.track("video0", "video");
	const audio = segmenter.track("audio0", "audio");

	// Audio groups 2..=4 never existed inside segment 0 (a gappy source).
	video.record(0, us(0));
	audio.record(0, us(0));
	audio.record(1, us(300));
	audio.record(5, us(1500));
	video.record(1, us(2000));
	audio.record(6, us(2100));

	expect(records[0]?.tracks?.audio0).toEqual([
		{ start: 0, end: 1 },
		{ start: 5, end: 5 },
	]);
});

test("a whole-segment gap omits the track", () => {
	const { segmenter, records } = capture();
	const video = segmenter.track("video0", "video");
	const audio = segmenter.track("audio0", "audio");

	video.record(0, us(0));
	audio.record(0, us(0));
	video.record(1, us(2000));
	video.record(2, us(4000));
	audio.record(1, us(4500));

	expect(records).toHaveLength(2);
	expect(records[1]).toEqual({ segment: 1, pts: 2000, duration: 2000, tracks: { video0: [{ start: 1, end: 1 }] } });
});

test("a non-keyframe range start is flagged", () => {
	const { segmenter, records } = capture();
	const video = segmenter.track("video0", "video");

	segmenter.cut(us(0));
	segmenter.cut(us(2000));
	video.record(0, us(0), true);
	// A gappy source resumes without an IDR.
	video.record(1, us(2500), false);
	video.record(2, us(4000), true);
	video.close();
	segmenter.finish();

	expect(records[1]?.tracks?.video0).toEqual([{ start: 1, end: 2, keyframe: false }]);
});

test("a closed track stops gating completeness", () => {
	const { segmenter, records } = capture();
	const video = segmenter.track("video0", "video");
	const audio = segmenter.track("audio0", "audio");

	video.record(0, us(0));
	audio.record(0, us(0));
	video.record(1, us(2000));
	video.record(2, us(4000));
	expect(records).toHaveLength(0); // audio gates both segments

	// Audio dies mid-broadcast: its close is what unblocks the flushes.
	audio.close();
	expect(records).toHaveLength(2);
	expect(records[1]?.tracks).toEqual({ video0: [{ start: 1, end: 1 }] });
});

test("records flushed before a sink attaches are buffered", () => {
	const segmenter = new Segmenter();
	const audio = segmenter.track("audio0", "audio");
	for (const [seq, ms] of [
		[0, 0],
		[1, 1000],
		[2, 2000],
		[3, 3000],
		[4, 4000],
	] as const) {
		audio.record(seq, us(ms));
	}

	const records: Record[] = [];
	segmenter.attach((record) => records.push(record));
	expect(records).toHaveLength(2);
	expect(records[0]).toEqual({ segment: 0, pts: 0, duration: 2000, tracks: { audio0: [{ start: 0, end: 1 }] } });
});

test("groups before the first boundary join the first segment", () => {
	const { segmenter, records } = capture();
	const video = segmenter.track("video0", "video");
	const audio = segmenter.track("audio0", "audio");

	// Audio races ahead of video's first keyframe (the startup race): its early groups belong
	// to segment 0, not to nowhere.
	audio.record(0, us(0));
	video.record(0, us(30));
	video.record(1, us(2030));
	audio.record(1, us(2100));

	expect(records).toEqual([
		{
			segment: 0,
			pts: 30, // the boundary is video's first keyframe; audio's earlier group still joins
			duration: 2000,
			tracks: { video0: [{ start: 0, end: 0 }], audio0: [{ start: 0, end: 0 }] },
		},
	]);
});
