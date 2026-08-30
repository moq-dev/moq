import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Broadcast, Time } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { subscribe } from "./subscription";

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test("audio subscription tracks the playback latency ceiling without restarting", async () => {
	const source = new Broadcast.Producer();
	source.createTrack("audio");
	const broadcast = source.consume();
	const maxLatency = new Signal<Time.Milli>(Time.Milli(38.75));
	const effect = new Effect();
	const subscriber = subscribe(effect, { broadcast, track: "audio", maxLatency });

	expect(subscriber.subscription.peek()).toEqual({
		priority: Catalog.PRIORITY.audio,
		ordered: false,
		latencyMax: 39,
		startGroup: undefined,
		endGroup: undefined,
	});

	await settle();
	maxLatency.set(Time.Milli(500.25));
	await settle();

	expect(subscriber.closed.peek()).toBeUndefined();
	expect(subscriber.subscription.peek()).toEqual({
		priority: Catalog.PRIORITY.audio,
		ordered: false,
		latencyMax: 501,
		startGroup: undefined,
		endGroup: undefined,
	});

	effect.close();
	expect(subscriber.closed.peek()).toBeNull();
	broadcast.close();
	source.close();
});
