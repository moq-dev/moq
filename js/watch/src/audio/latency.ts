import { Time } from "@moq/net";
import { type Latency, latencyBounds } from "../sync";

/** The inputs that determine whether audio needs a deeper playback cushion. */
export interface ReanchorFloor {
	/** The configured latency target. */
	latency: Latency;

	/** Additional delay required by the selected audio rendition. */
	audio?: Time.Milli;

	/** Additional delay required by the active or pending video rendition. */
	video?: Time.Milli;
}

/** The stable latency floor whose increase requires the audio ring to refill. */
export function reanchorFloor(props: ReanchorFloor): Time.Milli {
	const { min } = latencyBounds(props.latency);
	const target = min === "real-time" ? Time.Milli.zero : min;
	const media = Time.Milli.max(props.audio ?? Time.Milli.zero, props.video ?? Time.Milli.zero);
	return Time.Milli.add(target, media);
}
