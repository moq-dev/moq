import type * as Moq from "@moq/net";
import type { Time } from "@moq/net";
import type { Effect, Getter } from "@moq/signals";

/**
 * Open a media subscription with its max age on the initial request and every update.
 *
 * @internal
 */
export function subscribeMedia(
	effect: Effect,
	props: {
		broadcast: Moq.Broadcast.Consumer;
		track: string;
		priority: number;
		maxAge: Getter<Time.Milli>;
	},
): Moq.Track.Subscriber {
	const subscription = () => ({ priority: props.priority, maxAge: props.maxAge.peek() });
	const subscriber = props.broadcast.track(props.track).subscribe(subscription());
	effect.cleanup(() => subscriber.close());

	effect.run((inner) => {
		subscriber.update({ priority: props.priority, maxAge: inner.get(props.maxAge) });
	});

	return subscriber;
}
