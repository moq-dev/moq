import { Container } from "@moq/hang";
import type * as Moq from "@moq/net";
import { Error as NetError } from "@moq/net";
import { type Effect, type Getter, type GetterInit, getter } from "@moq/signals";

/**
 * Open a media subscription, joining live at its min delay, with its priority and delay on the
 * initial request and every update, and its max delay on every update. The serving hop starts the
 * live join that far behind the live edge, so a cold join, a replacement, or a fallback arrives with
 * its buffer full rather than replaying the retained backlog. An update keeps the ranges first
 * subscribed: a changed range resolves again, which would place the join anew and skip media
 * buffered behind it.
 *
 * @internal
 */
export function subscribeMedia(
	effect: Effect,
	props: {
		broadcast: Moq.Broadcast.Consumer;
		track: string;
		priority: GetterInit<number>;
		delay: Getter<Container.Delay>;
	},
): Moq.Track.Subscriber | undefined {
	if (effect.get(props.broadcast.closed) !== undefined) return;
	const priority = getter(props.priority);
	const { ranges } = Container.subscription(props.delay.peek());
	const subscription = (level: number, delay: Container.Delay) => ({
		priority: level,
		delay: Container.subscription(delay).delay,
		ranges,
	});
	const subscriber = props.broadcast.track(props.track).subscribe(subscription(priority.peek(), props.delay.peek()));
	effect.cleanup(() => subscriber.close());

	effect.run((inner) => {
		subscriber.update(subscription(inner.get(priority), inner.get(props.delay)));
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
