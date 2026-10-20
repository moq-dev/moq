import { Signal } from "@moq/signals";
import type { Demand } from "../track.ts";

/** Resolve once no subscriber wants the track, or it closed. */
export async function unwanted(demand: Demand): Promise<void> {
	while (demand.wanted.peek() !== undefined && demand.closed.peek() === undefined) {
		await Signal.race(demand.wanted, demand.closed);
	}
}
