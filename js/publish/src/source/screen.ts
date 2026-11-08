import { Derived, Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type * as Audio from "../audio";
import type * as Video from "../video";
import type { Media } from "./types";

/** Signals the screen capture reads. */
export type ScreenInput = {
	// Whether to hold the capture open. Defaults to true. Each enable prompts the user to pick a surface,
	// so construct an enabled screen source during a user gesture or start it later with a live input.
	enabled: Getter<boolean>;
};

/** Constructor options: the wired inputs plus the live-editable track constraints. */
export interface ScreenProps extends Inputs<ScreenInput> {
	/** Seed the video constraints; also editable via `screen.video` for the next picker. */
	video?: Video.Constraints | boolean | Signal<Video.Constraints | boolean | undefined>;
	/** Seed the audio constraints; also editable via `screen.audio` for the next picker. */
	audio?: Audio.Constraints | boolean | Signal<Audio.Constraints | boolean | undefined>;
}

/** Why a screen share ended. */
export type ScreenEnd =
	// The user stopped sharing through the browser, or the surface went away.
	| { reason: "stopped" }
	// The user dismissed the picker or refused permission.
	| { reason: "dismissed" }
	// The browser could not capture.
	| { reason: "error"; error: Error };

/**
 * The phase of a screen share. `ended` holds until the next enable, so it says why sharing stopped
 * even after the app switches it off in response.
 */
export type ScreenState =
	| { kind: "off" }
	| { kind: "prompting" }
	| { kind: "live"; media: Media }
	| ({ kind: "ended" } & ScreenEnd);

/** Signals the screen capture derives. */
export type ScreenOutput = {
	// The share's phase.
	state: Getter<ScreenState>;
	// The captured surface while live, otherwise undefined.
	source: Getter<Media | undefined>;
};

const OFF: ScreenState = { kind: "off" };

/** Captures a screen, window, or tab that the user picks. */
export class Screen {
	/** The wired inputs, read-only. */
	readonly in: Readonlys<ScreenInput>;

	/** The video constraints the next picker uses. Browsers require video, so false makes the picker fail. */
	video: Signal<Video.Constraints | boolean | undefined>;
	/** The audio constraints the next picker uses, or false to capture video only. */
	audio: Signal<Audio.Constraints | boolean | undefined>;

	readonly #state = new Signal<ScreenState>(OFF);

	/** The derived outputs, read-only. */
	readonly out: Readonlys<ScreenOutput> = readonlys({
		state: this.#state,
		source: new Derived([this.#state], (state) => (state.kind === "live" ? state.media : undefined)),
	});

	#signals = new Effect();

	constructor(props?: ScreenProps) {
		this.in = {
			enabled: getter(props?.enabled ?? true),
		};
		this.video = Signal.from(props?.video);
		this.audio = Signal.from(props?.audio);

		this.#signals.run(this.#run.bind(this));
	}

	// Reruns only when `enabled` changes, so each enable opens exactly one picker. The constraints
	// are read rather than tracked: a picker needs a user gesture, and editing them is not one.
	#run(effect: Effect): void {
		if (!effect.get(this.in.enabled)) return;

		this.#state.set({ kind: "prompting" });
		// Switching off or closing releases the share. An ended one keeps its reason.
		effect.cleanup(() => this.#state.update((state) => (state.kind === "ended" ? state : OFF)));

		// TODO Expose these to the application.
		// @ts-expect-error TypeScript's DOM lib lacks the Chrome-only `CaptureController`.
		let controller: CaptureController | undefined;
		// @ts-expect-error TypeScript's DOM lib lacks the Chrome-only `CaptureController`.
		if (typeof self.CaptureController !== "undefined") {
			// @ts-expect-error TypeScript's DOM lib lacks the Chrome-only `CaptureController`.
			controller = new CaptureController();
			controller.setFocusBehavior("no-focus-change");
		}

		// Called synchronously, so it still runs inside the gesture that enabled it; a synchronous throw
		// becomes a rejection like any other refusal.
		const pending = (async () =>
			navigator.mediaDevices.getDisplayMedia({
				video: this.video.peek(),
				audio: this.audio.peek(),
				// @ts-expect-error TypeScript's DOM lib lacks the Chrome-only `CaptureController`.
				controller,
				preferCurrentTab: false,
				selfBrowserSurface: "exclude",
				surfaceSwitching: "include",
				// TODO We should try to get system audio, but need to be careful about feedback.
				// systemAudio: "exclude",
			}))();

		// The picker may outlive this run; its eventual stream still belongs to the run.
		const release = () =>
			pending.then(
				(stream) => {
					for (const track of stream.getTracks()) track.stop();
				},
				() => {},
			);
		effect.cleanup(release);

		// Closing can land between the picker settling and this task resuming; the cleanup's state wins.
		const abort = effect.abort;
		effect.spawn(async () => {
			let media: MediaStream | undefined;
			try {
				media = await effect.race(pending);
			} catch (error) {
				if (abort.aborted) return;
				// The spec reports a refused or dismissed picker as NotAllowedError, and nothing else does.
				const end: ScreenEnd =
					error instanceof DOMException && error.name === "NotAllowedError"
						? { reason: "dismissed" }
						: { reason: "error", error: error instanceof Error ? error : new Error(String(error)) };
				this.#state.set({ kind: "ended", ...end });
				return;
			}
			if (!media || abort.aborted) return;

			const v = media.getVideoTracks().at(0) as Video.StreamTrack | undefined;
			const a = media.getAudioTracks().at(0) as Audio.StreamTrack | undefined;

			const stop = () => {
				this.#state.set({ kind: "ended", reason: "stopped" });
				void release();
			};

			// A track that arrives dead already fired "ended", so the listener below would never run.
			if (v?.readyState === "ended" || a?.readyState === "ended") return stop();

			// The browser's own "Stop sharing" control ends a track. Either one ending ends the share,
			// releasing its sibling too.
			for (const track of [v, a]) {
				if (track) effect.event(track, "ended", stop);
			}

			this.#state.set({
				kind: "live",
				media: {
					video: v
						? {
								track: v,
								get scale() {
									return (v.getSettings() as MediaTrackSettings & { screenPixelRatio?: number })
										.screenPixelRatio;
								},
							}
						: undefined,
					audio: a ? { track: a, kind: "music" } : undefined,
				},
			});
		});
	}

	/** Stop the capture and release the surface. */
	close() {
		this.#signals.close();
	}
}
