import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type * as Video from "../video";
import { Device, type DeviceProps } from "./device";

// Signals the camera reads.
export type CameraInput = {
	// Whether to hold the camera open. When false the track is stopped and `out.source` clears.
	enabled: Getter<boolean>;
};

/** Constructor options: the wired inputs, the live-editable constraints, and the device seed. */
export interface CameraProps extends Inputs<CameraInput> {
	/** Seed the device selection; also live-editable via `camera.device`. */
	device?: DeviceProps;
	/** Seed the capture constraints; also live-editable via `camera.constraints`. */
	constraints?: Video.Constraints | Signal<Video.Constraints | undefined>;
}

type CameraOutput = {
	// The live camera track, or undefined while disabled or denied.
	source: Signal<Video.Source | undefined>;
};

/** Captures video from a camera, tracking the available devices. */
export class Camera {
	// The browser picks a low default resolution (often 640x480), so request 720p.
	// Caller-supplied constraints take precedence per field.
	static readonly DEFAULT_CONSTRAINTS: Video.Constraints = {
		width: { ideal: 1280 },
		height: { ideal: 720 },
	};

	readonly in: Readonlys<CameraInput>;

	/** The available cameras and which one to use. */
	readonly device: Device<"video">;

	/** The live-editable capture constraints, merged over {@link DEFAULT_CONSTRAINTS}. */
	constraints: Signal<Video.Constraints | undefined>;

	readonly #out: CameraOutput = {
		source: new Signal<Video.Source | undefined>(undefined),
	};
	readonly out = readonlys(this.#out);

	#signals = new Effect();

	constructor(props?: CameraProps) {
		this.in = {
			enabled: getter(props?.enabled ?? false),
		};
		this.device = new Device("video", props?.device);
		this.constraints = Signal.from(props?.constraints);

		this.#signals.run(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		if (!enabled) return;

		const device = effect.get(this.device.out.requested);
		const constraints = effect.get(this.constraints) ?? {};

		// Build final constraints with device selection, defaulting resolution unless overridden.
		const finalConstraints: MediaTrackConstraints = {
			...Camera.DEFAULT_CONSTRAINTS,
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

			const source = stream.getVideoTracks()[0] as Video.Source | undefined;

			// getUserMedia resolved, so we have permission even if no track came back.
			effect.cleanup(this.device.capture(source?.getSettings().deviceId));
			if (!source) return;

			effect.set(this.#out.source, source);
		});
	}

	/** Stop the capture and release the device. */
	close() {
		this.#signals.close();
		this.device.close();
	}
}
