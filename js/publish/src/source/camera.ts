import { Effect, Signal } from "@moq/signals";
import type * as Video from "../video";
import { Device, type DeviceProps } from "./device";

export interface CameraProps {
	enabled?: boolean | Signal<boolean>;
	device?: DeviceProps;
	constraints?: Video.Constraints | Signal<Video.Constraints | undefined>;
}

export class Camera {
	// The browser picks a low default resolution (often 640x480), so request 720p.
	// Caller-supplied constraints take precedence per field.
	static readonly DEFAULT_CONSTRAINTS: Video.Constraints = {
		width: { ideal: 1280 },
		height: { ideal: 720 },
	};

	enabled: Signal<boolean>;
	device: Device<"video">;

	constraints: Signal<Video.Constraints | undefined>;

	source = new Signal<Video.Source | undefined>(undefined);

	// Bumped when the captured track ends underneath us (e.g. the webcam is unplugged),
	// so #run re-acquires instead of leaving a frozen source forever.
	#retry = new Signal(0);

	signals = new Effect();

	constructor(props?: CameraProps) {
		this.device = new Device("video", props?.device);
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

		// Build final constraints with device selection, defaulting resolution unless overridden.
		const finalConstraints: MediaTrackConstraints = {
			...Camera.DEFAULT_CONSTRAINTS,
			...constraints,
			deviceId: device ? { exact: device } : undefined,
		};

		effect.spawn(async () => {
			// A denied/cancelled permission prompt must not take down the effect, but stay visible:
			// the broadcast still announces, so watchers would otherwise buffer forever with no clue why.
			const media = navigator.mediaDevices.getUserMedia({ video: finalConstraints }).catch((err) => {
				console.warn("camera capture failed:", err);
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

			this.device.permission.set(true);

			const source = stream.getVideoTracks()[0] as Video.Source | undefined;
			if (!source) return;

			const settings = source.getSettings();

			// The track can end underneath us (device unplugged, OS revoked). Re-acquire so
			// capture moves to whatever device is now available.
			effect.event(source, "ended", () => {
				console.warn("camera track ended; re-acquiring");
				this.#retry.update((n) => n + 1);
			});

			effect.set(this.device.active, settings.deviceId);
			effect.set(this.source, source);
		});
	}

	close() {
		this.signals.close();
		this.device.close();
	}
}
