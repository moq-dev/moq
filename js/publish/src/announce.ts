import type { Effect, Getter, Signal } from "@moq/signals";
import type { AnnounceMode } from "./element";

// One captured track, as the announce gate sees it.
interface Track {
	enabled: Getter<boolean>;
	source: Getter<unknown>;
	// Whether the encoder's config resolved, or can't until something outside it changes.
	settled: Getter<boolean | undefined>;
}

export interface Props {
	// Undefined refuses to announce.
	mode: Getter<AnnounceMode | undefined>;
	// A source remains selected while the element is connected, even across capture gaps.
	selected: Getter<boolean | undefined>;
	// A camera captures video and audio as one publication.
	camera: Getter<boolean | undefined>;
	video: Track;
	audio: Track;
}

// Drive `<moq-publish>`'s announce from its `announce` mode.
//
// In `source` mode the first announce also waits until every enabled, captured track settles. The
// broadcast serves no catalog until it is announced, so its first snapshot lists every rendition: a
// one-shot consumer (export, HLS, a recording) can't reinitialize for a track that joins later. Once
// a camera broadcast is announced it stays announced while selected and connected, so a capture
// gap (a device swap, mute, or retry) only edits the catalog.
export function run(effect: Effect, announcing: Signal<boolean>, props: Props): void {
	effect.run((effect) => {
		const mode = effect.get(props.mode);
		const selected = !!effect.get(props.selected);
		const video = effect.get(props.video.source) !== undefined;
		const audio = effect.get(props.audio.source) !== undefined;
		const videoEnabled = effect.get(props.video.enabled);
		const audioEnabled = effect.get(props.audio.enabled);

		// A camera waits for every enabled track, so denying either permission can't advertise a
		// partial broadcast.
		const camera = effect.get(props.camera);
		const ready = !camera || ((!videoEnabled || video) && (!audioEnabled || audio));
		const live = (video || audio) && ready;
		const active = selected && (camera || live);

		const settled =
			(!videoEnabled || !video || !!effect.get(props.video.settled)) &&
			(!audioEnabled || !audio || !!effect.get(props.audio.settled));

		announcing.set(mode === "always" || (mode === "source" && active && (announcing.peek() || (live && settled))));
	});
}
