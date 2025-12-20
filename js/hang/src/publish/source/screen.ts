import { Effect, Signal } from "@moq/signals";
import type * as Audio from "../audio";
import type * as Video from "../video";

export interface ScreenProps {
	enabled?: boolean | Signal<boolean>;
	video?: Video.Constraints | boolean | Signal<Video.Constraints | boolean | undefined>;
	audio?: Audio.Constraints | boolean | Signal<Audio.Constraints | boolean | undefined>;
}

export class Screen {
	enabled: Signal<boolean>;

	video: Signal<Video.Constraints | boolean | undefined>;
	audio: Signal<Audio.Constraints | boolean | undefined>;

	source = new Signal<{ audio?: Audio.Source; video?: Video.Source } | undefined>(undefined);

	signals = new Effect();

	constructor(props?: ScreenProps) {
		this.enabled = Signal.from(props?.enabled ?? false);
		this.video = Signal.from(props?.video);
		this.audio = Signal.from(props?.audio);

		this.signals.effect(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const video = effect.get(this.video);
		const audio = effect.get(this.audio);

		// TODO Expose these to the application.
		// @ts-expect-error Chrome only
		let controller: CaptureController | undefined;
		// @ts-expect-error Chrome only
		if (typeof self.CaptureController !== "undefined") {
			// @ts-expect-error Chrome only
			controller = new CaptureController();
			controller.setFocusBehavior("no-focus-change");
		}

		effect.spawn(async () => {
			const media = await Promise.race([
				navigator.mediaDevices
					.getDisplayMedia({
						video,
						audio,
						// @ts-expect-error Chrome only
						controller,
						preferCurrentTab: false,
						selfBrowserSurface: "exclude",
						surfaceSwitching: "include",
						// TODO We should try to get system audio, but need to be careful about feedback.
						// systemAudio: "exclude",
					})
					.catch(() => undefined),
				effect.cancel,
			]);
			if (!media) return;

			const v = media.getVideoTracks().at(0) as Video.StreamTrack | undefined;
			const a = media.getAudioTracks().at(0) as Audio.StreamTrack | undefined;

			effect.cleanup(() => v?.stop());
			effect.cleanup(() => a?.stop());

			effect.set(this.source, { video: v, audio: a });
		});
	}

	close() {
		this.signals.close();
	}
}
