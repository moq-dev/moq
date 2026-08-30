import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import type { Time } from "@moq/net";
import type { Effect, Getter } from "@moq/signals";

/** Opens a live-edge audio subscription and keeps its expiry budget aligned with playback. @internal */
export function subscribe(
	effect: Effect,
	broadcast: Moq.Broadcast.Consumer,
	track: string,
	maxLatency: Getter<Time.Milli>,
): Moq.Track.Subscriber {
	const priority = Catalog.PRIORITY.audio;
	let latencyMax = maxLatency.peek();
	const subscriber = broadcast.track(track).subscribe({ priority, latencyMax });
	effect.cleanup(() => subscriber.close());

	effect.run((inner) => {
		const next = inner.get(maxLatency);
		if (next === latencyMax) return;
		latencyMax = next;
		subscriber.update({ priority, latencyMax });
	});

	return subscriber;
}
