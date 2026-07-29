import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type * as Audio from "../audio";
import type * as Video from "../video";

// Signals the screen capture reads.
export type ScreenInput = {
	// Whether to hold the capture open. Enabling it prompts the user to pick a surface.
	enabled: Getter<boolean>;
};

/** Constructor options: the wired inputs plus the live-editable track constraints. */
export interface ScreenProps extends Inputs<ScreenInput> {
	/** Seed the video constraints; also live-editable via `screen.video`. */
	video?: Video.Constraints | boolean | Signal<Video.Constraints | boolean | undefined>;
	/** Seed the audio constraints; also live-editable via `screen.audio`. */
	audio?: Audio.Constraints | boolean | Signal<Audio.Constraints | boolean | undefined>;
}

type ScreenOutput = {
	// The captured surface, or undefined while disabled or dismissed.
	source: Signal<{ audio?: Audio.Source; video?: Video.Source } | undefined>;
};

/** Captures a screen, window, or tab that the user picks. */
export class Screen {
	readonly in: Readonlys<ScreenInput>;

	/** The live-editable video constraints, or false to capture audio only. */
	video: Signal<Video.Constraints | boolean | undefined>;
	/** The live-editable audio constraints, or false to capture video only. */
	audio: Signal<Audio.Constraints | boolean | undefined>;

	readonly #out: ScreenOutput = {
		source: new Signal<{ audio?: Audio.Source; video?: Video.Source } | undefined>(undefined),
	};
	readonly out = readonlys(this.#out);

	#signals = new Effect();

	// Reruns #run once a track ends, so the run itself releases the share.
	readonly #rerun = new Signal(0);

	// Deliberately a plain field: effect reruns must not unwind it, or the share reopens itself.
	#stopped = false;

	constructor(props?: ScreenProps) {
		this.in = {
			enabled: getter(props?.enabled ?? false),
		};
		this.video = Signal.from(props?.video);
		this.audio = Signal.from(props?.audio);

		this.#signals.run(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		if (!enabled) {
			// Being switched off is the app's reset, so a later enable prompts again.
			this.#stopped = false;
			return;
		}

		effect.get(this.#rerun);

		// The share is over and cannot be reopened without another user gesture. Returning here is
		// what releases it: the previous run's cleanup stops every track and clears out.source.
		if (this.#stopped) return;

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

			// The browser's own "Stop sharing" control ends a track. Either one ending means the
			// share is over, and the tracks can end independently, so rerun into the stopped branch
			// rather than clearing out.source here: that runs the cleanup above and releases the
			// sibling track too.
			for (const track of [v, a]) {
				if (!track) continue;
				effect.event(track, "ended", () => {
					this.#stopped = true;
					this.#rerun.update((rerun) => rerun + 1);
				});
			}

			effect.set(this.#out.source, {
				video: v,
				audio: a ? { track: a, kind: "music" } : undefined,
			});
		});
	}

	/** Stop the capture and release the surface. */
	close() {
		this.#signals.close();
	}
}
