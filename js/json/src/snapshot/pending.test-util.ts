import type { Pending, Update } from "./encoder.ts";

/** The frame an update produced, or `undefined` when unchanged, for values that always fit the budget. */
export function pending(update: Update): Pending | undefined {
	if (update.kind === "tooLarge") throw new Error("the value is past the group budget");
	return update.kind === "frame" ? update.frame : undefined;
}
