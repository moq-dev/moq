import type { Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast";
import { Timestamp } from "./time.ts";
import { hooks } from "./internal.ts";

let probes = 0;

/** Whether `active` reads its tracks from `source` now: a group cached there arrives through it. @internal */
export function readsFrom(active: BroadcastConsumer | undefined, source: BroadcastProducer): boolean {
	if (!active) return false;
	const name = `probe-${probes++}`;
	const track = source.createTrack(name);
	const group = hooks.appendNext(track);
	group.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.unscaled(group.start) });
	group.finish();
	const sub = active.track(name).subscribe();
	try {
		return sub.tryRecvGroup() !== undefined;
	} finally {
		sub.close();
		track.finish();
		track.close();
		source.removeTrack(name);
	}
}
