import { expect, test } from "bun:test";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { subscribeMedia } from "./media";

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

test("media latency is present on the initial subscription and later updates", async () => {
	let initial: Moq.Track.Subscription | undefined;
	const updates: Moq.Track.Subscription[] = [];
	const subscriber = {
		close: () => undefined,
		update: (subscription: Moq.Track.Subscription) => updates.push(subscription),
	} as unknown as Moq.Track.Subscriber;
	const broadcast = {
		track: () => ({
			subscribe: (subscription: Moq.Track.Subscription) => {
				initial = subscription;
				return subscriber;
			},
		}),
	} as unknown as Moq.Broadcast.Consumer;
	const latency = new Signal(Time.Milli(250));
	const effect = new Effect();

	subscribeMedia(effect, {
		broadcast,
		track: "media",
		priority: 7,
		latency,
	});
	expect(initial).toEqual({ priority: 7, maxAge: 250 });

	latency.set(Time.Milli(500));
	await flush();
	expect(updates.at(-1)).toEqual({ priority: 7, maxAge: 500 });

	effect.close();
});
