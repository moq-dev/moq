/** The audible offset of an advancing resumed picture, excluding its silent preview. */
import { AUDIBLE_RMS, type Sample } from "./contract";
import * as Pattern from "./pattern";

export function resumeSkew(sample: Sample, previousFrame: number | undefined): number | undefined {
	if (
		previousFrame === undefined ||
		sample.frameId === undefined ||
		sample.frameId === previousFrame ||
		sample.toneStep === undefined ||
		sample.audioStalled ||
		(sample.rms ?? 0) < AUDIBLE_RMS
	)
		return undefined;
	return Pattern.stepSkew(sample.toneStep, Pattern.expectedStep(sample.frameId)) * Pattern.STEP_MS;
}
