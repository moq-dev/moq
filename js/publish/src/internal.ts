import { Micro, Milli } from "@moq/clock";
import type * as Hls from "@moq/hang/hls";
import type * as Moq from "@moq/net";
import type { Effect, Getter } from "@moq/signals";

/** @internal Terminal failures of broadcast-owned rendition tracks, including idle tracks. */
export const errors = new WeakMap<object, Getter<Error | undefined>>();

/**
 * @internal Whether a subscriber is attached to `track`, scoped to `effect`. Changes only when that
 * does, not with every group the subscribers want, so a bandwidth reservation keyed on it holds.
 */
export function watched(track: Moq.Track.Producer, effect: Effect): Getter<boolean | undefined> {
	return effect.computed((demand) => demand.get(track.demand().wanted) !== undefined);
}

/**
 * @internal Whether anyone wants `track`'s media: a subscriber to it, or to its HLS records, since an
 * export reads only the records.
 */
export function wanted(effect: Effect, track: Moq.Track.Producer, recorder: Hls.Recorder | undefined): boolean {
	if (effect.get(track.demand().wanted) !== undefined) return true;
	return recorder !== undefined && effect.get(recorder.track.demand().wanted) !== undefined;
}

/**
 * @internal A rendition's HLS duration hint for groups of `duration`. A zero or unbounded group still
 * hints a millisecond, which declares the grid's own duration up front: without a hint, an on-demand
 * rendition never advertises the records whose subscription would start it encoding.
 */
export function hlsHint(duration: Milli): Micro {
	return Micro.fromMilli(Number.isFinite(duration) && duration > 0 ? duration : Milli(1));
}
