/**
 * The recorder page: subscribes to a broadcast's audio and stamps each frame as it comes off its
 * group's stream, which is where the player's container consumer receives it.
 *
 * Built only on the public `@moq/net` and `@moq/hang` surface, so it records the path a viewer's
 * player takes (a WebTransport session in Chromium, every group on its own stream) without a debug
 * hook in the player. It reads groups the way `Container.Consumer` does, concurrently and decoded
 * with the same format, but stamps them before the consumer's in-order delivery: a group the
 * consumer holds back waiting for a missing one, or skips for age, is a decision the replay makes
 * again at its own delay, and baking one in would censor the trace.
 *
 *     ?url=https://cdn.moq.dev/demo&broadcast=bbb.hang
 *
 * `record.ts` drains it through `globalThis.recorder`.
 *
 * @module
 */
import * as Catalog from "@moq/hang/catalog";
import * as Moq from "@moq/net";
import { format, arrivals as readArrivals } from "./capture.ts";
import type { Arrival } from "./schema.ts";

/** How far back the relay may serve a group. Long, so a late group is recorded rather than dropped. */
const MAX_DELAY = Moq.Time.Milli(10_000);

/** What the recorder has, until the driver drains it. */
export type Recorder = {
	/** Arrivals since the last drain, `at` on this page's `performance.now()`. */
	drain(): Arrival[];
	/** The session and rendition, once subscribed. */
	info(): { transport: string; rtt: number | null; config: Catalog.AudioConfig } | undefined;
	/** The failure that stopped recording, if one did. */
	error(): string | undefined;
};

declare global {
	var recorder: Recorder;
}

const params = new URLSearchParams(location.search);
const required = (name: string): string => {
	const value = params.get(name);
	if (!value) throw new Error(`missing ?${name}`);
	return value;
};

const arrivals: Arrival[] = [];
let info: ReturnType<Recorder["info"]>;
let failure: string | undefined;
const fail = (err: unknown) => {
	failure ??= err instanceof Error ? err.message : String(err);
	console.error("recorder:", failure);
};

globalThis.recorder = {
	drain: () => arrivals.splice(0, arrivals.length),
	info: () => info,
	error: () => failure,
};

async function record(): Promise<void> {
	const origin = new Moq.Origin.Producer();
	const connection = await Moq.Connection.connect({ url: new URL(required("url")), consume: origin });

	let rtt: number | null = null;
	connection.probe.subscribe((probe) => {
		if (probe.rtt !== undefined) rtt = rtt === null ? probe.rtt : Math.min(rtt, probe.rtt);
	});

	const request = origin.request(Moq.Path.from(required("broadcast")));
	let broadcast = request.active.peek();
	while (!broadcast) {
		await request.active.changed();
		broadcast = request.active.peek();
	}

	let root: Catalog.Root | undefined;
	for await (const update of Catalog.watch(broadcast)) {
		if (Object.keys(update.audio?.renditions ?? {}).length > 0) {
			root = update;
			break;
		}
	}
	// Every recorded broadcast carries one audio rendition, so the first is the one a player picks.
	const [name, config] = Object.entries(root?.audio?.renditions ?? {})[0] ?? [];
	if (!name || !config) throw new Error("the catalog ended without an audio rendition");

	const decoder = format(config);
	const track = broadcast.track(name).subscribe({ priority: Catalog.PRIORITY.audio, maxDelay: MAX_DELAY });
	info = {
		transport: connection.transport,
		get rtt() {
			return rtt;
		},
		config,
	};

	for (;;) {
		const group = await track.recvGroup();
		if (!group) throw new Error("the track ended");
		(async () => {
			for await (const arrival of readArrivals(group, decoder)) arrivals.push(arrival);
		})().catch(fail);
	}
}

record().catch(fail);
