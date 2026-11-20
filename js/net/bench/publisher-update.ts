/**
 * Sweep what a lite-07 SUBSCRIBE_UPDATE costs the publisher as the subscription's ranges grow.
 *
 * One subscription holds `ranges` one-group ranges on a track with no group yet, so nothing is
 * served. Each update retires the `edited` oldest ranges and adds as many past the newest, so the
 * union moves with every edit; `steer` moves only the priority. A cost that grows with
 * the live ranges rather than the edit shows up as a slope over `ranges`. Mirrors the Rust
 * `publisher_update` bench.
 *
 * Run with `just js bench publisher-update`.
 */
import { randomHop } from "../src/hop.ts";
import { Publisher } from "../src/lite/publisher.ts";
import { Subscribe, SubscribeUpdate, type WireRange } from "../src/lite/subscribe.ts";
import { ALPN_07_WIP, Version } from "../src/lite/version.ts";
import { createMockTransportPair } from "../src/mock.ts";
import { Producer as OriginProducer } from "../src/origin.ts";
import * as Path from "../src/path.ts";
import { Stream } from "../src/stream.ts";
import { Timescale } from "../src/time.ts";

/** Live ranges in the subscription. */
const LIVE = [16, 1024, 8192];
/** Ranges each update retires, and adds. */
const EDITED = [1, 16];
/** Updates timed per run, beyond the run with none that measures teardown. */
const UPDATES = 1024;
const REPS = 7;

// One group per range, cycling over twice the live ranges so every key fits the SUBSCRIBE's varints.
const range = (ranges: number, index: bigint): WireRange => {
	const group = index % BigInt(2 * ranges);
	return { start: { time: group }, end: { time: group } };
};

// Serve one subscription of `ranges` ranges, send it `updates` updates, and time the whole run.
async function run(ranges: number, edited: number | "steer", updates: number): Promise<number> {
	const origin = new OriginProducer();
	const broadcast = origin.route(Path.from("bench")).broadcast;
	broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null });
	const pair = createMockTransportPair(ALPN_07_WIP);
	const publisher = new Publisher(pair.server, Version.DRAFT_07, randomHop(), origin.consume());
	const client = await Stream.open(pair.client, { version: Version.DRAFT_07 });
	const server = await Stream.accept(pair.server, Version.DRAFT_07);
	if (!server) throw new Error("missing subscribe stream");
	const serving = publisher.runSubscribe(
		new Subscribe({
			id: 0n,
			broadcast: Path.from("bench"),
			track: "video",
			priority: 0,
			maxDelay: 3_600_000,
			order: "ascending",
			ranges: Array.from({ length: ranges }, (_, index) => range(ranges, BigInt(index))),
		}),
		server,
	);
	// Timed once the subscription is served and parked, so only the updates and teardown count.
	await new Promise((resolve) => setTimeout(resolve, 100));
	const start = performance.now();
	// The Range IDs of the oldest live range and the next one added, and of the next range's group.
	let oldest = 0n;
	let next = BigInt(ranges);
	for (let step = 0; step < updates; step++) {
		const update =
			edited === "steer"
				? new SubscribeUpdate({
						priority: step % 2,
						order: "ascending",
						maxDelay: 3_600_000,
						remove: [],
						ranges: [],
					})
				: new SubscribeUpdate({
						priority: 0,
						order: "ascending",
						maxDelay: 3_600_000,
						remove: Array.from({ length: edited }, (_, index) => oldest + BigInt(index)),
						ranges: Array.from({ length: edited }, (_, index) => range(ranges, next + BigInt(index))),
					});
		if (edited !== "steer") {
			oldest += BigInt(edited);
			next += BigInt(edited);
		}
		await update.encode(client.writer, Version.DRAFT_07);
	}
	client.writer.close();
	await serving;
	const elapsed = performance.now() - start;
	publisher.close();
	client.close();
	pair.client.close();
	pair.server.close();
	broadcast.close();
	origin.close();
	return elapsed;
}

const debug = console.debug;
console.debug = () => {};
try {
	console.log("ranges,edit,update_us");
	for (const ranges of LIVE) {
		for (const edited of [...EDITED, "steer" as const]) {
			// The fastest of each, since a shared host only ever adds time.
			let base = Number.POSITIVE_INFINITY;
			let timed = Number.POSITIVE_INFINITY;
			for (let rep = 0; rep < REPS; rep++) {
				base = Math.min(base, await run(ranges, edited, 0));
				timed = Math.min(timed, await run(ranges, edited, UPDATES));
			}
			const label = edited === "steer" ? "steer" : `edit${edited}`;
			console.log(`${ranges},${label},${(((timed - base) * 1000) / UPDATES).toFixed(1)}`);
		}
	}
} finally {
	console.debug = debug;
}
