import { expect, test } from "bun:test";
import { Container } from "@moq/hang";
import * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, Once, Signal } from "@moq/signals";
import { nextMedia, subscribeMedia } from "./media";

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

test("media max age is present on the initial subscription and later updates", async () => {
	let initial: Moq.Track.Subscription | undefined;
	const updates: Moq.Track.Subscription[] = [];
	const subscriber = {
		close: () => undefined,
		update: (subscription: Moq.Track.Subscription) => updates.push(subscription),
	} as unknown as Moq.Track.Subscriber;
	const broadcast = {
		closed: new Once<Error | null>(),
		track: () => ({
			subscribe: (subscription: Moq.Track.Subscription) => {
				initial = subscription;
				return subscriber;
			},
		}),
	} as unknown as Moq.Broadcast.Consumer;
	const maxAge = new Signal(Time.Milli(250));
	const effect = new Effect();

	subscribeMedia(effect, {
		broadcast,
		track: "media",
		priority: 7,
		maxAge,
	});
	expect(initial).toEqual({ priority: 7, maxAge: Time.Milli(250) });

	maxAge.set(Time.Milli(500));
	await flush();
	expect(updates.at(-1)).toEqual({ priority: 7, maxAge: Time.Milli(500) });

	effect.close();
});

for (const end of [
	new Moq.Error.Stream(Moq.StreamCode.Cancel),
	new Moq.Error.Stream(Moq.StreamCode.Internal),
	new Moq.Error.Stream(Moq.StreamCode(1234)),
	new Moq.Error.Session(Moq.SessionCode.Internal),
	new Error("decoder failed"),
]) {
	test(`media subscription end: ${end}`, async () => {
		const track = new Moq.Track.Producer("test");
		const consumer = new Container.Consumer(track.subscribe(), { format: new Container.Legacy.Format("data") });
		try {
			const pending = nextMedia(consumer);
			track.close(end);
			if (end instanceof Moq.Error.Stream) expect(await pending).toBeUndefined();
			else await expect(pending).rejects.toBe(end);
		} finally {
			consumer.close();
		}
	});
}

test("media does not subscribe through a closed broadcast handle", () => {
	const broadcast = new Moq.Broadcast.Producer();
	const handle = broadcast.consume();
	broadcast.close();
	const effect = new Effect();
	try {
		expect(
			subscribeMedia(effect, {
				broadcast: handle,
				track: "video",
				priority: 0,
				maxAge: new Signal(Time.Milli(0)),
			}),
		).toBeUndefined();
	} finally {
		effect.close();
		handle.close();
	}
});
