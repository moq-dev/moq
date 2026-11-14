import type { Container } from "@moq/hang";
import type * as Moq from "@moq/net";
import { Error as NetError } from "@moq/net";
import type { Effect, Getter } from "@moq/signals";

/**
 * Open a media subscription with its delay on the initial request and every update.
 *
 * @internal
 */
export function subscribeMedia(
	effect: Effect,
	props: {
		broadcast: Moq.Broadcast.Consumer;
		track: string;
		priority: number;
		delay: Getter<Moq.Track.Delay>;
	},
): Moq.Track.Subscriber | undefined {
	if (effect.get(props.broadcast.closed) !== undefined) return;
	const subscription = () => ({ priority: props.priority, delay: props.delay.peek() });
	const subscriber = props.broadcast.track(props.track).subscribe(subscription());
	effect.cleanup(() => subscriber.close());

	effect.run((inner) => {
		subscriber.update({ priority: props.priority, delay: inner.get(props.delay) });
	});

	return subscriber;
}

/** A container read, or the publisher finishing the track once every read is delivered. @internal */
export type MediaRead = Container.Read | { kind: "finish" };

/**
 * Read the next media item. Undefined once the subscription is reset or `signal`, the reader's
 * owner, closed it: either way nothing says the media ended.
 * @internal
 */
export async function nextMedia(consumer: Container.Consumer, signal: AbortSignal): Promise<MediaRead | undefined> {
	try {
		const read = await consumer.next();
		// A reader only reads as finished on its own when the publisher finished the track cleanly.
		if (read || signal.aborted) return read;
		return { kind: "finish" };
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
