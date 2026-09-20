import type { Effect, Signal } from "@moq/signals";
import type * as Audio from "./audio";
import type * as Source from "./source";
import type * as Video from "./video";

/** The mutable source state owned by the publish element. */
export interface SourceState {
	holders: {
		video: Signal<Source.Camera | Source.Screen | undefined>;
		audio: Signal<Source.Microphone | Source.Screen | undefined>;
		file: Signal<Source.File | undefined>;
	};
	video: Signal<Video.Source | undefined>;
	audio: Signal<Audio.Source | undefined>;
}

/** Clears every holder and captured source for the lifetime of this effect run. */
export function clearSourceState(effect: Effect, state: SourceState): void {
	effect.set(state.holders.video, undefined);
	effect.set(state.holders.audio, undefined);
	effect.set(state.holders.file, undefined);
	effect.set(state.video, undefined);
	effect.set(state.audio, undefined);
}
