/** Publisher and consumer roles for the @moq/net stream path over a real WebTransport session. */
import { Milli } from "@moq/clock";
import * as Moq from "@moq/net";
import { check, payload } from "../fixture.ts";
import { longTasks, nextTask } from "./observe.ts";

/** One transport workload: `frames` frames of `size` bytes, `group` frames per group. */
export interface Transport {
	url: string;
	path: string;
	size: number;
	frames: number;
	group: number;
	/** Check every byte instead of sampled bytes; used by the counted pass only. */
	full: boolean;
}

/** What one consumer run delivered and what it cost inside the page. */
export interface Delivery {
	frames: number;
	bytes: number;
	digest: number;
	elapsed_ms: number;
	long_tasks: { count: number; total_ms: number };
}

const cleanup: (() => void)[] = [];
let writer: Promise<void> | undefined;
let broadcast: Moq.Broadcast.Consumer | undefined;

async function publish(config: Transport): Promise<void> {
	// Fixture generation stays outside every timed interval.
	const fixtures = Array.from({ length: config.frames }, (_, frame) => payload(config.size, frame));
	const origin = new Moq.Origin.Producer();
	const publication = origin.createBroadcast(Moq.Path.from(config.path));
	const producer = publication.broadcast;
	// Retention covers the whole run, so a slow consumer cannot lose a group to eviction.
	const track = producer.createTrack("data", { timescale: Moq.Time.Timescale.MILLI, maxAge: Milli(600_000) });
	const demand = track.demand().wanted;
	publication.announce();
	const connection = await Moq.Connection.connect({
		url: new URL(config.url),
		publish: origin.consume(),
		websocket: { enabled: false },
	});
	if (connection.transport !== "webtransport") throw new Error(`publisher negotiated ${connection.transport}`);
	cleanup.push(() => {
		track.finish();
		track.close();
		producer.close();
		connection.abort();
		origin.close();
	});
	writer = (async () => {
		while (demand.peek() === undefined && track.closed.peek() === undefined)
			await Promise.race([demand.changed(), track.closed]);
		if (track.closed.peek() !== undefined) return;
		for (let start = 0; start < config.frames; start += config.group) {
			const group = track.appendGroup();
			for (let frame = start; frame < Math.min(start + config.group, config.frames); frame++)
				group.writeFrame({ payload: fixtures[frame], timestamp: Moq.Time.Timestamp.fromMillis(frame) });
			group.finish();
		}
	})();
}

/** Connects and waits for the announcement, so the timed run starts at SUBSCRIBE. */
async function prepare(config: Transport): Promise<string> {
	const origin = new Moq.Origin.Producer();
	const connection = await Moq.Connection.connect({
		url: new URL(config.url),
		consume: origin,
		websocket: { enabled: false },
	});
	if (connection.transport !== "webtransport") throw new Error(`consumer negotiated ${connection.transport}`);
	const request = origin.follow(Moq.Path.from(config.path));
	cleanup.push(() => {
		request.close();
		connection.abort();
		origin.close();
	});
	for (;;) {
		broadcast = request.active.peek();
		if (broadcast) return connection.version;
		const closed = request.closed.peek();
		if (closed !== undefined) throw closed ?? new Error("broadcast request closed");
		await Promise.race([request.active.changed(), request.closed]);
	}
}

async function consume(config: Transport): Promise<Delivery> {
	if (!broadcast) throw new Error("consume before prepare");
	const groups = Math.ceil(config.frames / config.group);
	const start = performance.now();
	const subscriber = broadcast.track("data").subscribe({ maxDelay: Milli(600_000), order: "ascending" });
	cleanup.push(() => subscriber.close());
	let frames = 0;
	let bytes = 0;
	let digest = 0;
	const seen = new Set<number>();
	const readers: Promise<void>[] = [];
	for (let received = 0; received < groups; received++) {
		const group = await subscriber.recvGroup();
		if (!group) throw new Error(`track ended after ${received} of ${groups} groups`);
		if (group.sequence >= groups || seen.has(group.sequence)) throw new Error(`unexpected group ${group.sequence}`);
		seen.add(group.sequence);
		readers.push(
			(async () => {
				const first = group.sequence * config.group;
				const last = Math.min(first + config.group, config.frames);
				try {
					for (let frame = first; frame < last; frame++) {
						const got = await group.readFrame();
						if (!got) throw new Error(`group ${group.sequence} ended before frame ${frame}`);
						digest = (digest + check(got.payload, config.size, frame, config.full)) >>> 0;
						frames++;
						bytes += got.payload.byteLength;
					}
					if (await group.readFrame()) throw new Error(`group ${group.sequence} has an extra frame`);
				} finally {
					group.close();
				}
			})(),
		);
	}
	await Promise.all(readers);
	const end = performance.now();
	// The final reads may still be one long task; its entry only exists once that task ends.
	await nextTask();
	return { frames, bytes, digest, elapsed_ms: end - start, long_tasks: longTasks(start, end) };
}

async function stop(): Promise<void> {
	for (const close of cleanup.splice(0)) close();
	await writer;
	writer = undefined;
	broadcast = undefined;
}

declare global {
	interface Window {
		transport: { publish: typeof publish; prepare: typeof prepare; consume: typeof consume; stop: typeof stop };
	}
}
window.transport = { publish, prepare, consume, stop };
