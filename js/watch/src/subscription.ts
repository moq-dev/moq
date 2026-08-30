import type * as Moq from "@moq/net";
import type { Time } from "@moq/net";
import type { Effect, Getter } from "@moq/signals";

/** Opens a media subscription and keeps its expiry budget aligned with playback. @internal */
export function subscribe(
	effect: Effect,
	{
		broadcast,
		track,
		priority,
		maxLatency,
	}: {
		broadcast: Moq.Broadcast.Consumer;
		track: string;
		priority: number;
		maxLatency: Getter<Time.Milli>;
	},
): Moq.Track.Subscriber {
	let latencyMax = Math.ceil(maxLatency.peek());
	const subscriber = broadcast.track(track).subscribe({ priority, latencyMax });
	effect.cleanup(() => subscriber.close());

	effect.run((inner) => {
		const next = Math.ceil(inner.get(maxLatency));
		if (next === latencyMax) return;
		latencyMax = next;
		subscriber.update({ priority, latencyMax });
	});

	return subscriber;
}
