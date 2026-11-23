import { expect, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import { Container } from "@moq/hang";
import * as Moq from "@moq/net";
import { Effect, Once, Signal } from "@moq/signals";
import { nextMedia, subscribeMedia } from "./media";

const flush = () => yieldNow();

test("media joins live at its min delay, and retunes only the join's max", async () => {
	let initial: Moq.Track.Subscription | undefined;
	const updates: Moq.Track.Subscription[] = [];
	const current = new Signal<Moq.Track.Subscription | undefined>(undefined);
	const subscriber = {
		close: () => undefined,
		subscription: current,
		update: (subscription: Moq.Track.Subscription) => {
			updates.push(subscription);
			current.set(subscription);
		},
	} as unknown as Moq.Track.Subscriber;
	const broadcast = {
		closed: new Once<Error | null>(),
		track: () => ({
			subscribe: (subscription: Moq.Track.Subscription) => {
				initial = subscription;
				current.set(subscription);
				return subscriber;
			},
		}),
	} as unknown as Moq.Broadcast.Consumer;
	const delay = new Signal<Container.Delay>({ min: Duration.millis(100), max: Duration.millis(250) });
	const effect = new Effect();

	subscribeMedia(effect, {
		broadcast,
		track: "media",
		priority: 7,
		delay,
	});
	// A live join its min delay behind the live edge, under its max.
	const live = (max: number) => [{ start: { live: { min: Duration.millis(100), max: Duration.millis(max) } } }];
	expect(initial).toEqual({ priority: 7, ranges: live(250) });

	delay.set({ min: Duration.millis(100), max: Duration.millis(500) });
	await flush();
	expect(updates.at(-1)).toEqual({ priority: 7, ranges: live(500) });

	// A new min moves no join: the range resolves once, and placing it anew would skip media
	// buffered behind the new start.
	delay.set({ min: Duration.millis(300), max: Duration.millis(500) });
	await flush();
	expect(updates.at(-1)).toEqual({ priority: 7, ranges: live(500) });

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
			const pending = nextMedia(consumer, new AbortController().signal);
			track.abort(end);
			if (end instanceof Moq.Error.Stream) expect(await pending).toBeUndefined();
			else await expect(pending).rejects.toBe(end);
		} finally {
			consumer.close();
		}
	});
}

// Only the publisher's clean finish says the media ended; its owner closing the reader does not.
test("media reads a clean finish, but not its own close, as the end", async () => {
	const track = new Moq.Track.Producer("test");
	const finished = new Container.Consumer(track.subscribe(), { format: new Container.Legacy.Format("data") });
	const owner = new AbortController();
	const closed = new Container.Consumer(track.subscribe(), { format: new Container.Legacy.Format("data") });
	try {
		const pending = nextMedia(finished, new AbortController().signal);
		const closing = nextMedia(closed, owner.signal);
		owner.abort();
		closed.close();
		expect(await closing).toBeUndefined();
		track.finish();
		expect(await pending).toEqual({ kind: "finish" });
	} finally {
		finished.close();
		closed.close();
	}
});

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
				delay: new Signal<Container.Delay>({}),
			}),
		).toBeUndefined();
	} finally {
		effect.close();
		handle.close();
	}
});
