import { Signal } from "@moq/signals";
import type { Demand } from "../track.ts";

/** Resolve once no subscriber wants the track, or it closed. */
export async function unwanted(demand: Demand): Promise<void> {
	while (demand.wanted.peek() !== undefined && demand.closed.peek() === undefined) {
		await Signal.race(demand.wanted, demand.closed);
	}
}

/**
 * Resolve once demand that arrived for the track leaves, or it closed. An info-only lookup
 * attaches no subscriber, so demand that never arrived is not abandonment.
 */
export async function abandoned(demand: Demand): Promise<void> {
	while (demand.wanted.peek() === undefined && demand.closed.peek() === undefined) {
		await Signal.race(demand.wanted, demand.closed);
	}
	for (;;) {
		await unwanted(demand);
		if (demand.closed.peek() !== undefined || demand.wanted.peek() === undefined) return;
	}
}
