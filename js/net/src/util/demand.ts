import { Signal } from "@moq/signals";
import { hooks } from "../internal.ts";
import type { Demand } from "../track.ts";

/** Resolve once neither a subscriber nor a held info lookup wants the track, or it closed. */
export async function unwanted(demand: Demand): Promise<void> {
	while (demand.wanted.peek() && hooks.demandReleased(demand).peek() === undefined) {
		await Signal.race(demand.wanted, hooks.demandReleased(demand));
	}
}

/** Resolve once demand that arrived for the track leaves, or it closed: demand that never arrived is not abandonment. */
export async function abandoned(demand: Demand): Promise<void> {
	while (!demand.wanted.peek() && hooks.demandReleased(demand).peek() === undefined) {
		await Signal.race(demand.wanted, hooks.demandReleased(demand));
	}
	for (;;) {
		await unwanted(demand);
		if (isAbandoned(demand)) return;
	}
}

/** Whether neither a subscriber nor a held info lookup wants the track now, or it closed. */
export function isAbandoned(demand: Demand): boolean {
	return hooks.demandReleased(demand).peek() !== undefined || !demand.wanted.peek();
}
