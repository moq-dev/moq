import { describe, expect, test } from "bun:test";
import { VTTCue } from "media-captions";
import { insertCue, rollUp } from "./renderer";

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
