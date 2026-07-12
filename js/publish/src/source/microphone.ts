import { Effect, Signal } from "@moq/signals";
import type * as Audio from "../audio";
import { Device, type DeviceProps } from "./device";

export interface MicrophoneProps {
	enabled?: boolean | Signal<boolean>;
	device?: DeviceProps;
	constraints?: Audio.Constraints | Signal<Audio.Constraints | undefined>;
}

export class Microphone {
	enabled: Signal<boolean>;

	device: Device<"audio">;

	constraints: Signal<Audio.Constraints | undefined>;

	source = new Signal<Audio.Source | undefined>(undefined);

	// Bumped when the captured track ends underneath us (e.g. a Bluetooth headset disconnects),
	// so #run re-acquires instead of encoding silence off a dead track forever.
	#retry = new Signal(0);

	signals = new Effect();

	constructor(props?: MicrophoneProps) {
		this.device = new Device("audio", props?.device);

		this.enabled = Signal.from(props?.enabled ?? false);
		this.constraints = Signal.from(props?.constraints);

		this.signals.run(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		effect.get(this.#retry);
		const device = effect.get(this.device.requested);

		const constraints = effect.get(this.constraints) ?? {};
		const finalConstraints: MediaTrackConstraints = {
			...constraints,
			deviceId: device !== undefined ? { exact: device } : undefined,
		};

		effect.spawn(async () => {
			// A denied/cancelled permission prompt must not take down the effect, but stay visible:
			// the broadcast still announces, so watchers would otherwise buffer forever with no clue why.
			const media = navigator.mediaDevices.getUserMedia({ audio: finalConstraints }).catch((err) => {
				console.warn("microphone capture failed:", err);
				return undefined;
			});

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

			// Success, we can enumerate devices now.
			this.device.permission.set(true);

			const track = stream.getAudioTracks()[0] as Audio.StreamTrack | undefined;
			if (!track) return;

			const settings = track.getSettings();

			if (device === undefined) {
				// Save the device that the user selected during the dialog prompt.
				this.device.preferred.set(settings.deviceId);
			}

			// The track can end underneath us (device unplugged, Bluetooth dropped, OS revoked).
			// Re-acquire so capture moves to whatever device is now available.
			effect.event(track, "ended", () => {
				console.warn("microphone track ended; re-acquiring");
				this.#retry.update((n) => n + 1);
			});

			effect.set(this.device.active, settings.deviceId);
			effect.set(this.source, { track, kind: "voice" });
		});
	}

	close() {
		this.signals.close();
		this.device.close();
	}
}
