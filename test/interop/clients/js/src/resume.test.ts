import { expect, test } from "bun:test";
import type { Sample } from "./contract";
import * as Pattern from "./pattern";
import { resumeSkew } from "./resume";

const state = (frameId: number, toneStep: number, audioStalled = false) =>
	({ frameId, toneStep, audioStalled, rms: 0.35 }) as Sample;

test("the first advancing audible picture exposes stale audio", () => {
	const sample = state(1, Pattern.expectedStep(1) + 8);
	expect(Math.abs(resumeSkew(sample, 0) ?? 0)).toBeGreaterThan(Pattern.STEP_MS);
	expect(resumeSkew(state(1, Pattern.expectedStep(1)), 0)).toBe(0);
});

test("a held preview and buffering silence are not presented AV pairs", () => {
	expect(resumeSkew(state(1, 8), undefined)).toBeUndefined();
	expect(resumeSkew(state(1, 8), 1)).toBeUndefined();
	expect(resumeSkew(state(1, 8, true), 0)).toBeUndefined();
	expect(resumeSkew({ ...state(1, 8), rms: 0 }, 0)).toBeUndefined();
});
