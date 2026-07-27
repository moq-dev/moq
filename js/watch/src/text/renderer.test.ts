import { describe, expect, test } from "bun:test";
import { VTTCue } from "media-captions";
import { clearCue, insertCue, pruneCues, rollUp } from "./renderer";

// Build a cue store by inserting in the given delivery order, returning the resulting start times.
function starts(order: number[]): number[] {
	const cues: VTTCue[] = [];
	for (const start of order) insertCue(cues, new VTTCue(start, start + 1, `cue ${start}`));
	return cues.map((cue) => cue.startTime);
}

describe("insertCue", () => {
	test("keeps cues sorted when they arrive in order", () => {
		expect(starts([0, 1, 2, 3])).toEqual([0, 1, 2, 3]);
	});

	// Groups are handed out in delivery order, not sequence order, so a cue can land before one
	// already stored. Appending blindly would render captions out of order.
	test("keeps cues sorted when they arrive out of order", () => {
		expect(starts([2, 0, 3, 1])).toEqual([0, 1, 2, 3]);
	});

	test("keeps equal start times in arrival order", () => {
		const cues: VTTCue[] = [];
		insertCue(cues, new VTTCue(1, 2, "first"));
		insertCue(cues, new VTTCue(1, 2, "second"));
		expect(cues.map((c) => c.text)).toEqual(["first", "second"]);
	});
});

describe("rollUp", () => {
	// A utf8 cue has no real end: it runs until the next one starts.
	test("ends the previous cue where this one begins", () => {
		const cues: VTTCue[] = [];
		const first = new VTTCue(0, 30, "first");
		insertCue(cues, first);
		rollUp(cues, first);

		const second = new VTTCue(2, 32, "second");
		insertCue(cues, second);
		rollUp(cues, second);

		expect(first.endTime).toBe(2);
		expect(second.endTime).toBe(32);
	});

	// The successor may already have arrived, in which case the late cue is the one to clamp.
	test("clamps a late cue against the successor already stored", () => {
		const cues: VTTCue[] = [];
		const later = new VTTCue(4, 34, "later");
		insertCue(cues, later);
		rollUp(cues, later);

		const earlier = new VTTCue(1, 31, "earlier");
		insertCue(cues, earlier);
		rollUp(cues, earlier);

		expect(earlier.endTime).toBe(4);
		expect(later.startTime).toBe(4);
		expect(later.endTime).toBe(34);
	});

	test("leaves a lone cue lingering", () => {
		const cues: VTTCue[] = [];
		const only = new VTTCue(0, 30, "only");
		insertCue(cues, only);
		rollUp(cues, only);
		expect(only.endTime).toBe(30);
	});
});

describe("clearCue", () => {
	// The hang draft defines an empty utf8 payload as clearing the caption, so it has to be
	// scheduled rather than dropped: otherwise stale accessibility text stays on screen.
	test("ends the cue showing at the clear time", () => {
		const cues = [new VTTCue(0, 30, "showing")];
		clearCue(cues, 5);
		expect(cues[0].endTime).toBe(5);
		expect(cues).toHaveLength(1);
	});

	test("ignores cues that have not started yet", () => {
		const cues = [new VTTCue(0, 2, "done"), new VTTCue(10, 40, "later")];
		clearCue(cues, 5);
		expect(cues[0].endTime).toBe(2);
		expect(cues[1].endTime).toBe(40);
	});

	test("is a no-op with nothing showing", () => {
		const cues: VTTCue[] = [];
		expect(() => clearCue(cues, 5)).not.toThrow();
	});
});

describe("pruneCues", () => {
	test("drops expired cues and reports the change", () => {
		const cues = [new VTTCue(0, 1, "old"), new VTTCue(2, 3, "old"), new VTTCue(9, 10, "fresh")];
		expect(pruneCues(cues, 5)).toBe(true);
		expect(cues.map((c) => c.text)).toEqual(["fresh"]);
		expect(pruneCues(cues, 5)).toBe(false);
	});

	// Cues are ordered by start time, not end time. A long-running early cue used to stop the
	// sweep at the head, letting every expired cue behind it accumulate for the whole stream.
	test("prunes behind a long-running early cue", () => {
		const cues = [new VTTCue(0, 1_000, "long")];
		for (let i = 1; i <= 20; i++) cues.push(new VTTCue(i, i + 0.5, `short ${i}`));

		expect(pruneCues(cues, 15)).toBe(true);
		expect(cues.map((c) => c.text)).toEqual([
			"long",
			"short 15",
			"short 16",
			"short 17",
			"short 18",
			"short 19",
			"short 20",
		]);
	});
});
