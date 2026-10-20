import type { Milli } from "@moq/clock";
import type { Container } from "@moq/hang";
import type * as Moq from "@moq/net";
import { Error as NetError } from "@moq/net";
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
		maxDelay: Getter<Milli>;
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

/** The source a fatal decode failure belongs to. @internal */
export interface Failure {
	/** The catalog broadcast's instance. */
	broadcast: unknown;
	/** The instance of the broadcast the rendition reads. */
	media: unknown;
	track: string;
	identity: string;
}

/**
 * Whether the selection moved off a failed source. A rendition that is only unavailable (muted by
 * its publisher, or its broadcast override offline) keeps the failure, so it is not retried.
 *
 * @internal
 */
export function failureMoved(failure: Failure, selection: Partial<Failure>): boolean {
	if (selection.broadcast !== failure.broadcast) return true;
	if (selection.track === undefined || selection.identity === undefined) return false;
	if (selection.track !== failure.track || selection.identity !== failure.identity) return true;
	return selection.media !== undefined && selection.media !== failure.media;
}
