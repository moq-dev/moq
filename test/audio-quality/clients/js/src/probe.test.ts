import { expect, mock, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Effect, Signal } from "@moq/signals";
import type MoqWatch from "@moq/watch/element";
import { Observed } from "../../../../../js/net/src/util/observed.ts";

// This test has no audio graph. Vite supplies this loader in the real page.
mock.module("./tap-worklet.ts?worklet", () => ({ default: async () => "" }));
const { probe } = await import("./probe.ts");

test("audio-only sampling holds probe demand until its final RTT sample", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const network = new Observed<{ rtt: Duration } | undefined>(undefined);
	const producer = new Effect((effect) => {
		if (effect.get(network.wanted)) effect.set(network, { rtt: Duration.millis(12) }, undefined);
	});
	const empty = new Signal(undefined);
	const watch = {
		connection: { probe: network },
		audio: {
			out: { stalled: new Signal(false), context: empty, root: empty, timestamp: empty, stats: empty },
			source: { out: { config: empty } },
		},
		sync: { out: { delay: new Signal(Duration.ZERO), jitter: empty }, now: () => undefined },
		broadcast: { out: { catalog: empty } },
	} as unknown as MoqWatch;
	const measurement = probe(watch);
	try {
		await clock.advance(Duration.millis(250));
		expect(measurement.drain().map((sample) => sample.rtt)).toEqual([12]);
		await measurement.finish();
		expect(measurement.drain().at(-1)?.rtt).toBe(12);
		expect(network.wanted.peek()).toBe(false);
		await clock.advance(Duration.millis(500));
		expect(measurement.drain()).toEqual([]);
	} finally {
		producer.close();
		installed.close();
	}
});
