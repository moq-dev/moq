import type { Effect, Getter, Signal } from "@moq/signals";
import type { AnnounceMode, SourceType } from "./element";

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
	// The selected source while the element is connected; it stays set across capture gaps.
	source: Getter<SourceType | File | undefined>;
	video: Track;
	audio: Track;
}

// Drive `<moq-publish>`'s announce from its `announce` mode.
//
// In `source` mode the first announce also waits until every enabled, captured track settles. The
// broadcast serves no catalog until it is announced, so its first snapshot lists every rendition: a
// one-shot consumer (export, HLS, a recording) can't reinitialize for a track that joins later. Once
// a camera broadcast is announced it stays announced while that camera stays selected and connected,
// so a capture gap (a device swap, mute, or retry) only edits the catalog.
export function run(effect: Effect, announcing: Signal<boolean>, props: Props): void {
	// The source the current announcement was made for; switching to another source must earn its own.
	let latched: SourceType | File | undefined;

	effect.run((effect) => {
		const mode = effect.get(props.mode);
		const source = effect.get(props.source);
		const videoEnabled = effect.get(props.video.enabled);
		const audioEnabled = effect.get(props.audio.enabled);
		// A muted or hidden track keeps capturing but publishes nothing, so it counts as absent.
		const video = videoEnabled && effect.get(props.video.source) !== undefined;
		const audio = audioEnabled && effect.get(props.audio.source) !== undefined;

		// A camera captures video and audio as one publication, so it waits for every enabled track and
		// denying either permission can't advertise a partial broadcast.
		const camera = source === "camera";
		const ready = !camera || ((!videoEnabled || video) && (!audioEnabled || audio));
		const live = (video || audio) && ready;
		const active = source !== undefined && (camera || live);

		const settled = (!video || !!effect.get(props.video.settled)) && (!audio || !!effect.get(props.audio.settled));

		const announced =
			mode === "always" || (mode === "source" && active && (latched === source || (live && settled)));
		latched = announced ? source : undefined;
		announcing.set(announced);
	});
}
