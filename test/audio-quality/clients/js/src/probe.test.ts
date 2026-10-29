import { expect, mock, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import { Effect, Signal } from "@moq/signals";
import type MoqWatch from "@moq/watch/element";
import { Observed } from "../../../../../js/net/src/util/observed.ts";

// This test has no audio graph. Vite supplies this loader in the real page.
mock.module("./tap-worklet.ts?worklet", () => ({ default: async () => "" }));
const { probe } = await import("./probe.ts");

test("audio-only sampling holds probe demand until its final RTT sample", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const network = new Observed<{ rtt: Milli } | undefined>(undefined);
	const producer = new Effect((effect) => {
		if (effect.get(network.used)) effect.set(network, { rtt: Milli(12) }, undefined);
	});
	const empty = new Signal(undefined);
	const watch = {
		player: {
			in: { probe: network },
			audio: {
				out: { stalled: new Signal(false), context: empty, root: empty, timestamp: empty },
				source: { out: { config: empty } },
			},
			sync: { out: { delay: empty, jitter: empty } },
			broadcast: { out: { catalog: empty } },
		},
	} as unknown as MoqWatch;
	const measurement = probe(watch);
	try {
		await clock.advance(Nano.fromMilli(Milli(250)));
		expect(measurement.drain().map((sample) => sample.rtt)).toEqual([12]);
		await measurement.finish();
		expect(measurement.drain().at(-1)?.rtt).toBe(12);
		expect(network.used.peek()).toBe(false);
		await clock.advance(Nano.fromMilli(Milli(500)));
		expect(measurement.drain()).toEqual([]);
	} finally {
		producer.close();
		installed.close();
	}
});
