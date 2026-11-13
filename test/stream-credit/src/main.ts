/**
 * The browser reader: one WebTransport session to the relay, subscribed to every track of the
 * broadcast at its tier's priority, reading each group's frames as they arrive.
 *
 * The driver calls {@link Window.subscribe} first, publishes once it resolves, then waits on
 * {@link Window.report} for every subscription to end.
 *
 * @module
 */
import * as Moq from "@moq/net";
import type { Plan, Received, Report, Track } from "./plan.ts";

declare global {
	interface Window {
		/** Dial `plan.url` and subscribe to every track, resolving once each subscription is live. */
		subscribe(plan: Plan): Promise<void>;
		/** Resolve once every subscription ended, with what each one read. */
		report(): Promise<Report>;
		/** What each subscription still reading has read so far. */
		pending(): Received[];
	}
}

let reading: Promise<Report> | undefined;
// Each subscription's reads so far, filled in as they happen.
const progress: Received[] = [];

window.subscribe = async (plan) => {
	const origin = new Moq.Origin.Producer();
	const connection = await Moq.Connection.connect({
		url: new URL(plan.url),
		versions: [plan.version],
		consume: origin,
		// The browser's own stream cap is the point, and only WebTransport has one.
		websocket: { enabled: false },
	});
	const request = origin.request(Moq.Path.from(plan.broadcast));
	let broadcast = request.active.peek();
	while (!broadcast) broadcast = await request.active.changed();
	const active = broadcast;

	const subscribers = plan.tracks.map((track) =>
		active.track(track.name).subscribe({
			priority: track.priority,
			delay: { max: Moq.Clock.Duration.millis(track.budget) },
		}),
	);
	// Each subscription is a stream the browser opens; its info arrives once the relay answers.
	await Promise.all(subscribers.map((subscriber) => subscriber.info()));

	const started = performance.now();
	reading = (async () => {
		const tracks = await Promise.all(plan.tracks.map((track, i) => read(track, subscribers[i], started)));
		const closed = connection.closed;
		await connection.close();
		return { version: connection.version, tracks, closed: String((await closed) ?? "") || null };
	})();
};

window.report = () => {
	if (!reading) throw new Error("subscribe first");
	return reading;
};

window.pending = () => progress.filter((received) => received.at === undefined);

// Read every group the subscription hands out until it ends. Frames are read concurrently with
// the next group, since a group paused for credit must not hold back the groups after it.
async function read(track: Track, subscriber: Moq.Track.Subscriber, started: number): Promise<Received> {
	const received: Received = { name: track.name, groups: [], ended: null };
	progress.push(received);
	const frames: Promise<void>[] = [];
	try {
		for (;;) {
			const group = await subscriber.recvGroup();
			if (!group) break;
			const entry: Received["groups"][number] = { sequence: Number(group.sequence), frames: [] };
			received.groups.push(entry);
			frames.push(
				(async () => {
					try {
						for (;;) {
							const frame = await group.readString();
							if (frame === undefined) break;
							entry.frames.push(frame);
						}
						entry.at = performance.now() - started;
					} catch (err) {
						entry.error = String(err);
					}
				})(),
			);
		}
	} catch (err) {
		received.ended = String(err);
	}
	await Promise.all(frames);
	received.at = performance.now() - started;
	return received;
}
