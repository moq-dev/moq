import { Effect, Signal } from "@moq/signals";
import type * as Video from "../video";
import { Device, type DeviceProps } from "./device";

export interface CameraProps {
	enabled?: boolean | Signal<boolean>;
	device?: DeviceProps;
	constraints?: Video.Constraints | Signal<Video.Constraints | undefined>;
}

export class Camera {
	enabled: Signal<boolean>;
	device: Device<"video">;

	constraints: Signal<Video.Constraints | undefined>;

	source = new Signal<Video.Source | undefined>(undefined);
	signals = new Effect();

	constructor(props?: CameraProps) {
		this.device = new Device("video", props?.device);
		this.enabled = Signal.from(props?.enabled ?? false);
		this.constraints = Signal.from(props?.constraints);

		this.signals.effect(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const device = effect.get(this.device.requested);
		const constraints = effect.get(this.constraints) ?? {};

		// Build final constraints with device selection
		const finalConstraints: MediaTrackConstraints = {
			...constraints,
			deviceId: device ? { exact: device } : undefined,
		};

		effect.spawn(async () => {
			const media = navigator.mediaDevices.getUserMedia({ video: finalConstraints }).catch(() => undefined);

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

			effect.set(this.device.active, settings.deviceId);
			effect.set(this.source, source);
		});
	}

	close() {
		this.signals.close();
		this.device.close();
	}
}
