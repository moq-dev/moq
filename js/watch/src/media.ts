import type { Container } from "@moq/hang";
import type * as Moq from "@moq/net";
import { Error as NetError, type Time } from "@moq/net";
import type { Effect, Getter } from "@moq/signals";

/**
 * Open a media subscription with its max delay on the initial request and every update.
 *
 * @internal
 */
export function subscribeMedia(
	effect: Effect,
	props: {
		broadcast: Moq.Broadcast.Consumer;
		track: string;
		priority: number;
		maxDelay: Getter<Time.Milli>;
	},
): Moq.Track.Subscriber | undefined {
	if (effect.get(props.broadcast.closed) !== undefined) return;
	const subscription = () => ({ priority: props.priority, maxDelay: props.maxDelay.peek() });
	const subscriber = props.broadcast.track(props.track).subscribe(subscription());
	effect.cleanup(() => subscriber.close());

	effect.run((inner) => {
		subscriber.update({ priority: props.priority, maxDelay: inner.get(props.maxDelay) });
	});

	return subscriber;
}

/** Read the next media frame, ending playback when its subscription is reset. @internal */
export async function nextMedia(consumer: Container.Consumer) {
	try {
		return await consumer.next();
	} catch (err) {
		if (!(err instanceof NetError.Stream)) throw err;
		// The subscription is over, even when other tracks on the session are still live.
		console.debug("media subscription ended", err);
		return undefined;
	}
}
