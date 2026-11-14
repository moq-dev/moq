/**
 * Sweep a reader's fill probes, and its hand-outs of live groups between them, over the received
 * stale runs it cannot trust, and the readers.
 */
import { Duration } from "@moq/clock";
import { Producer as BroadcastProducer } from "../src/broadcast.ts";
import { hooks, staleHole } from "../src/internal.ts";
import { Timestamp } from "../src/time.ts";

console.debug = () => {};
console.log("runs,readers,step_us");
for (const runs of [250, 1000, 4000, 16000]) {
	for (const readers of [1, 8]) {
		const broadcast = new BroadcastProducer();
		const track = broadcast.createTrack("video", { maxAge: null });
		// 2 and 3 are read up front, so the only gaps any fill is asked for are the drops below.
		for (const sequence of [2n, 3n]) {
			const live = track.createGroup(sequence);
			live.writeFrame({ payload: new Uint8Array([0]), timestamp: Timestamp.unscaled(live.sequence) });
			live.finish();
		}
		const requests = track.requests();
		let asked = 0;
		const serving = (async () => {
			for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) asked++;
		})();
		const subs = Array.from({ length: readers }, () =>
			track.subscribe({
				// Capped, so the upstream's verdicts, judged under an open range, settle none of them.
				ranges: [{ start: { absolute: 2n }, end: { absolute: BigInt(8 + 2 * runs) } }],
				delay: { max: Duration.millis(30_000) },
			}),
		);
		for (const sub of subs) {
			sub.tryRecvGroup();
			sub.tryRecvGroup();
		}
		const start = performance.now();
		// One run per drop, each probed by every reader before the next: the upstream judged them
		// under a wider budget, so no reader trusts them and each is asked of a fill. A live group
		// follows each, handed out while every fill asked so far is still out.
		for (let i = 0; i < runs; i++) {
			const sequence = BigInt(4 + 2 * i);
			hooks.dropTrackGroups(track, sequence, sequence + 1n, staleHole(60_000));
			for (const sub of subs) sub.tryRecvGroup();
			const group = track.createGroup(sequence + 1n);
			group.writeFrame({ payload: new Uint8Array([0]), timestamp: Timestamp.unscaled(group.sequence) });
			group.finish();
			for (const sub of subs) {
				if (sub.tryRecvGroup()?.sequence !== sequence + 1n)
					throw new Error(`group ${sequence + 1n} not handed out`);
			}
		}
		const elapsed = performance.now() - start;
		// The handler takes the queued fills once this turn yields. The readers join one per run.
		await new Promise((resolve) => setTimeout(resolve, 0));
		for (const sub of subs) sub.close();
		requests.close();
		await serving;
		if (asked !== runs) throw new Error(`asked ${asked} fills, expected ${runs}`);
		console.log(`${runs},${readers},${((elapsed * 1000) / (runs * readers)).toFixed(2)}`);
		broadcast.close();
	}
}
