import { type Dispose, Signal } from "@moq/signals";
import { StreamCode, StreamError } from "../error.ts";
import { hooks } from "../internal.ts";
import type { Demand, Request } from "../track.ts";

/** Resolve once no subscriber wants the track, or it closed. */
export async function unwanted(demand: Demand): Promise<void> {
	while (demand.wanted.peek() !== undefined && hooks.demandReleased(demand).peek() === undefined) {
		await Signal.race(demand.wanted, hooks.demandReleased(demand));
	}
}

/**
 * Resolve once demand that arrived for the track leaves, or it closed. An info-only lookup
 * attaches no subscriber, so demand that never arrived is not abandonment.
 */
export async function abandoned(demand: Demand): Promise<void> {
	while (demand.wanted.peek() === undefined && hooks.demandReleased(demand).peek() === undefined) {
		await Signal.race(demand.wanted, hooks.demandReleased(demand));
	}
	for (;;) {
		await unwanted(demand);
		if (isAbandoned(demand)) return;
	}
}

/** Resolve once neither a subscriber nor an info lookup wants the request, or its track closed. */
export async function unrequested(request: Request): Promise<void> {
	const wanted = hooks.requestWanted(request);
	const released = hooks.demandReleased(request.demand());
	while (wanted.peek() && released.peek() === undefined) await Signal.race(wanted, released);
}

/** Whether no subscriber wants the track now, or it closed. */
export function isAbandoned(demand: Demand): boolean {
	return hooks.demandReleased(demand).peek() !== undefined || demand.wanted.peek() === undefined;
}

/**
 * Abort with a cancel once nobody wants the track, watching until stopped, so a request setup
 * the publisher never answers ends with its last subscriber. `setup` aborts at the same moment,
 * before a stream that opens meanwhile can send on it.
 */
export function whileWanted(demand: Demand, setup: AbortController): { signal: AbortSignal; stop: Dispose } {
	const left = new AbortController();
	let unwatch: Dispose | undefined;
	const watch = () => {
		if (!isAbandoned(demand)) {
			unwatch = Signal.race(demand.wanted, hooks.demandReleased(demand)).changed(watch);
			return;
		}
		const cancel = new StreamError(StreamCode.Cancel, { message: "cancel" });
		left.abort(cancel);
		setup.abort(cancel);
	};
	watch();
	return { signal: left.signal, stop: () => unwatch?.() };
}
