import * as Moq from "@moq/net";
import { payload, type Workload } from "./fixture.ts";

const cleanup: (() => void)[] = [];
let writers: Promise<void>[] = [];

async function prepare(config: Workload): Promise<void> {
	const fixtures = Array.from({ length: config.frames }, (_, frame) => payload(config.size, frame));
	for (let index = 0; index < config.publishers; index++) {
		const origin = new Moq.Origin.Producer();
		const broadcastPublication = origin.createBroadcast(Moq.Path.from(`${config.id}/${index}`));
		const broadcast = broadcastPublication.broadcast;
		const track = broadcast.createTrack("data", { timescale: Moq.Time.Timescale.MILLI });
		const demand = track.demand().wanted;
		broadcastPublication.announce();
		const connection = await Moq.Connection.connect({
			url: new URL(config.url),
			publish: origin.consume(),
			websocket: { enabled: false },
		});
		cleanup.push(() => {
			track.close();
			broadcast.close();
			connection.abort();
			origin.close();
		});
		writers.push(
			(async () => {
				while (demand.peek() === undefined && track.closed.peek() === undefined)
					await Promise.race([demand.changed(), track.closed]);
				if (track.closed.peek() !== undefined) return;
				const group = new Moq.Group.Producer(config.sequence);
				for (const [frame, bytes] of fixtures.entries())
					group.writeFrame({ payload: bytes, timestamp: Moq.Time.Timestamp.fromMillis(frame) });
				group.close();
				track.writeGroup(group);
				// Keep the track and broadcast served until every reader completes. No timing arm or tail-close race.
			})(),
		);
	}
}

async function stop(): Promise<void> {
	for (const close of cleanup.splice(0)) close();
	await Promise.all(writers);
	writers = [];
}

declare global {
	interface Window {
		baselinePublish: typeof prepare;
		baselineStop: typeof stop;
	}
}
window.baselinePublish = prepare;
window.baselineStop = stop;
