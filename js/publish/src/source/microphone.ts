import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type * as Audio from "../audio";
import { Device, type DeviceProps } from "./device";
import { Restart } from "./restart";

// Signals the microphone reads.
export type MicrophoneInput = {
	// Whether to hold the microphone open. When false the track is stopped and `out.source` clears.
	enabled: Getter<boolean>;
};

/** Constructor options: the wired inputs, the live-editable constraints, and the device seed. */
export interface MicrophoneProps extends Inputs<MicrophoneInput> {
	/** Seed the device selection; also live-editable via `microphone.device`. */
	device?: DeviceProps;
	/** Seed the capture constraints; also live-editable via `microphone.constraints`. */
	constraints?: Audio.Constraints | Signal<Audio.Constraints | undefined>;
}

type MicrophoneOutput = {
	// The live microphone track, or undefined while disabled or denied.
	source: Signal<Audio.Source | undefined>;
};

/** Captures audio from a microphone, tracking the available devices. */
export class Microphone {
	readonly in: Readonlys<MicrophoneInput>;

	/** The available microphones and which one to use. */
	readonly device: Device<"audio">;

	/** The live-editable capture constraints. */
	constraints: Signal<Audio.Constraints | undefined>;

	readonly #out: MicrophoneOutput = {
		source: new Signal<Audio.Source | undefined>(undefined),
	};
	readonly out = readonlys(this.#out);

	#signals = new Effect();
	#restart = new Restart();

	constructor(props?: MicrophoneProps) {
		this.in = {
			enabled: getter(props?.enabled ?? false),
		};
		this.device = new Device("audio", props?.device);
		this.constraints = Signal.from(props?.constraints);

		this.#signals.run(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		if (!enabled) return;

		this.#restart.subscribe(effect);

		const device = effect.get(this.device.out.requested);

		const constraints = effect.get(this.constraints) ?? {};
		const finalConstraints: MediaTrackConstraints = {
			...constraints,
			deviceId: device ? { exact: device } : undefined,
		};

		effect.spawn(async () => {
			const media = navigator.mediaDevices.getUserMedia({ audio: finalConstraints }).catch(() => undefined);

			// If the effect is cancelled for any reason (ex. cancel), stop any media that we got.
			effect.cleanup(() =>
				media.then((media) =>
					media?.getTracks().forEach((track) => {
						track.stop();
					}),
				),
			);

			const stream = await Promise.race([media, effect.cancel]);
			if (!stream) return;

			const track = stream.getAudioTracks()[0] as Audio.StreamTrack | undefined;
			const settings = track?.getSettings();

			// getUserMedia resolved, so we have permission even if no track came back.
			effect.cleanup(this.device.capture(settings?.deviceId));
			if (!track) return;

			// A track that arrives dead has already fired "ended", so publishing it would strand a
			// silent capture that nothing retries.
			if (track.readyState === "ended") return;

			this.#restart.watch(effect, track);
			effect.set(this.#out.source, { track, kind: "voice" });
		});
	}

	/** Stop the capture and release the device. */
	close() {
		this.#signals.close();
		this.device.close();
	}
}
