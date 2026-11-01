/** Sweep buffered groups, frames per group, publishers and players through the container consumer. */
import { Milli } from "@moq/clock";
import { type Group, Time, Track, Varint } from "@moq/net";
import { Signal } from "@moq/signals";
import { Consumer, type Delay } from "../src/container/consumer.ts";
import { Format } from "../src/container/legacy.ts";

// Far past every fixture's span, so only the burst case skips anything.
const HOLD = Milli(1_000_000_000);
// One group per second at 30 fps.
const FRAME_US = 33_333;
const GROUP_US = 1_000_000;
const PAYLOAD = new Uint8Array([7]);
// Measured operations per case, repeating smaller ones.
const MIN_OPS = 8192;

type Scenario = "steady" | "lookahead" | "sparse" | "burst";

interface Player {
	track: Track.Producer;
	consumer: Consumer;
	// Every group the consumer received, to check that what it skipped is closed.
	received: Group.Consumer[];
}

function timestamp(group: number, frame: number): number {
	return group * GROUP_US + frame * FRAME_US;
}

function payload(group: number, frame: number): Uint8Array {
	const prefix = Varint.encode(timestamp(group, frame));
	const data = new Uint8Array(prefix.byteLength + PAYLOAD.byteLength);
	data.set(prefix, 0);
	data.set(PAYLOAD, prefix.byteLength);
	return data;
}

function wire(group: number, frame: number): Group.Frame {
	return { payload: payload(group, frame), timestamp: Time.Timestamp.fromMicros(timestamp(group, frame)) };
}

function open(track: Track.Producer, delay: Signal<Delay>): Player {
	const subscriber = track.subscribe({ maxDelay: HOLD });
	const received: Group.Consumer[] = [];
	const recv = subscriber.recvGroup.bind(subscriber);
	subscriber.recvGroup = async () => {
		const group = await recv();
		if (group) received.push(group);
		return group;
	};
	return { track, consumer: new Consumer(subscriber, { format: new Format("video"), delay }), received };
}

// The deterministic arrival order of the sparse case: every group exactly once, shuffled.
function shuffled(count: number): number[] {
	const order = Array.from({ length: count }, (_, i) => i);
	let seed = 42;
	for (let i = count - 1; i > 0; i--) {
		seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
		const j = seed % (i + 1);
		[order[i], order[j]] = [order[j], order[i]];
	}
	return order;
}

// Write one frame to every publisher, then wait until every player decoded it, as each arrival would
// be decoded before the next lands. A decoded frame always changes some buffered range.
async function arrive(groups: Group.Producer[], players: Player[], index: number, frame: number): Promise<void> {
	const changed = players.map((player) => player.consumer.buffered.changed());
	for (const group of groups) group.writeFrame(wire(index, frame));
	await Promise.all(changed);
}

// Read every buffered frame, checking timestamp order, and count the gaps between them.
async function drain(player: Player, expected: number[]): Promise<number> {
	let gaps = 0;
	for (const want of expected) {
		for (;;) {
			const read = await player.consumer.next();
			if (!read) throw new Error("frame missing");
			if (read.kind === "gap") {
				gaps++;
				continue;
			}
			if (read.kind !== "frame" || read.frame.timestamp !== want || read.frame.payload[0] !== PAYLOAD[0])
				throw new Error("frame missing or reordered");
			break;
		}
	}
	return gaps;
}

function ranges(player: Player): string {
	return JSON.stringify(player.consumer.buffered.peek());
}

/** Return elapsed ns, the measured operations, gaps and warnings for one case. */
async function measure(
	scenario: Scenario,
	groups: number,
	frames: number,
	publishers: number,
	readers: number,
): Promise<{ ns: number; ops: number; gaps: number; warnings: number }> {
	const delay = new Signal<Delay>({ max: HOLD });
	// Stamped like a hang publisher's media, so the transport finds its live edge at the newest group.
	const tracks = Array.from({ length: publishers }, (_, i) =>
		new Track.Producer(`p${i}`).accept({ timescale: Time.Timescale.MICRO }),
	);
	const players = tracks.flatMap((track) => Array.from({ length: readers }, () => open(track, delay)));
	let warnings = 0;
	const warn = console.warn;
	console.warn = () => {
		warnings++;
	};
	try {
		const sequence = (i: number) => (scenario === "sparse" ? i * 2 : i);
		const order = scenario === "sparse" ? shuffled(groups) : Array.from({ length: groups }, (_, i) => i);
		const expected = Array.from({ length: groups * frames }, (_, i) =>
			timestamp(sequence(Math.floor(i / frames)), i % frames),
		);
		let gaps = 0;
		let start = performance.now();
		if (scenario === "steady") {
			// The player reads each frame as it lands, so nothing builds up behind it.
			for (const i of order) {
				const sources = tracks.map((track) => track.createGroup(i));
				for (let frame = 0; frame < frames; frame++) {
					const reads = players.map((player) => player.consumer.next());
					for (const source of sources) source.writeFrame(wire(i, frame));
					for (const read of await Promise.all(reads))
						if (read?.kind !== "frame" || read.frame.timestamp !== timestamp(i, frame))
							throw new Error("frame missing or reordered");
				}
				for (const source of sources) source.finish();
			}
			return { ns: (performance.now() - start) * 1e6, ops: groups * frames * players.length, gaps, warnings };
		}

		// Everything arrives before the player reads: a stalled reader, or history after a join.
		for (const i of order) {
			const sources = tracks.map((track) => track.createGroup(sequence(i)));
			for (let frame = 0; frame < frames; frame++) await arrive(sources, players, sequence(i), frame);
			for (const source of sources) source.finish();
		}
		// A finished track proves no missing group can still arrive.
		if (scenario === "sparse") for (const track of tracks) track.finish();
		await Promise.resolve();

		for (const player of players) {
			// Finished adjacent groups merge into one range; sparse ones stay one range per group.
			const want =
				scenario === "sparse"
					? Array.from({ length: groups }, (_, i) => ({
							start: timestamp(sequence(i), 0) / 1000,
							end: timestamp(sequence(i), frames - 1) / 1000,
						}))
					: [{ start: 0, end: timestamp(groups - 1, frames - 1) / 1000 }];
			if (ranges(player) !== JSON.stringify(want)) throw new Error(`wrong buffered ranges: ${ranges(player)}`);
		}

		if (scenario === "burst") {
			// The budget drops to nothing, so the next arrival makes every older group too stale to keep.
			delay.set({ max: Milli.zero });
			start = performance.now();
			const sources = tracks.map((track) => track.createGroup(groups));
			await arrive(sources, players, groups, 0);
			const elapsed = (performance.now() - start) * 1e6;
			const newest = timestamp(groups, 0) / 1000;
			for (const player of players) {
				if (ranges(player) !== JSON.stringify([{ start: newest, end: newest }]))
					throw new Error(`skip left ${ranges(player)}`);
				if (player.received.slice(0, groups).some((group) => !group.isClosed))
					throw new Error("skipped group left open");
				const read = await player.consumer.next();
				if (read?.kind !== "frame" || read.frame.timestamp !== timestamp(groups, 0))
					throw new Error("newest frame missing");
			}
			return { ns: elapsed, ops: groups * players.length, gaps, warnings };
		}

		for (const player of players) gaps += await drain(player, expected);
		return { ns: (performance.now() - start) * 1e6, ops: groups * frames * players.length, gaps, warnings };
	} finally {
		console.warn = warn;
		for (const player of players) player.consumer.close();
		for (const track of tracks) track.close();
	}
}

/**
 * Print one CSV row per case. `ns_per_op` is per frame and player, except for a burst, where it is per
 * skipped group and player and covers only the arrival that skips them.
 */
export async function run(): Promise<void> {
	console.log("scenario,groups,frames,publishers,players,ops,gaps,warnings,ns_per_op");
	const cases: [Scenario, number, number, number, number][] = [];
	for (const scenario of ["steady", "lookahead", "sparse", "burst"] as const)
		for (const groups of [1, 16, 256, 1024])
			for (const frames of [1, 30]) cases.push([scenario, groups, frames, 1, 1]);
	for (const publishers of [1, 4])
		for (const players of [1, 8]) cases.push(["lookahead", 64, 30, publishers, players]);
	for (const [scenario, groups, frames, publishers, players] of cases) {
		// Small cases repeat, so a coarse browser clock still resolves them.
		const total = { ns: 0, ops: 0, gaps: 0, warnings: 0 };
		while (total.ops < MIN_OPS) {
			const { ns, ops, gaps, warnings } = await measure(scenario, groups, frames, publishers, players);
			total.ns += ns;
			total.ops += ops;
			total.gaps += gaps;
			total.warnings += warnings;
		}
		console.log(
			`${scenario},${groups},${frames},${publishers},${players},${total.ops},${total.gaps},${total.warnings},${(total.ns / total.ops).toFixed(0)}`,
		);
	}
}

if (import.meta.main) await run();
