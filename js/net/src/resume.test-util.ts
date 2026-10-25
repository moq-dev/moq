import type { Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast";

let probes = 0;

/** Whether `active` reads its tracks from `source` now: a group cached there arrives through it. @internal */
export function readsFrom(active: BroadcastConsumer | undefined, source: BroadcastProducer): boolean {
	if (!active) return false;
	const name = `probe-${probes++}`;
	const track = source.createTrack(name);
	const group = track.appendGroup();
	group.writeFrame({ payload: new Uint8Array([1]) });
	group.finish();
	const sub = active.track(name).subscribe();
	try {
		return sub.tryRecvGroup() !== undefined;
	} finally {
		sub.close();
		track.close();
		source.removeTrack(name);
	}
}
