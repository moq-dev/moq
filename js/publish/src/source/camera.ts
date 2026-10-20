import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type * as Video from "../video";
import { Device, type DeviceProps } from "./device";
import { Retry } from "./retry";
import type { Media } from "./types";

// Signals the camera reads.
export type CameraInput = {
	// Whether to hold the camera open. Defaults to true. When false the track is stopped and `out.source` clears.
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
	source: Signal<Media | undefined>;
	// Clockwise turn a landscape picture needs to appear upright on this screen: 90 or 270 while the
	// screen is portrait, otherwise 0. Feed it to a Video.Capture, which checks the live frames.
	orientation: Signal<number>;
	/** A terminal capture failure, cleared when a new capture attempt begins. */
	error: Signal<Error | undefined>;
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

	/**
	 * The live-editable capture constraints, merged over {@link DEFAULT_CONSTRAINTS}.
	 *
	 * `facingMode` picks the front or rear camera, unless `device.preferred` names one that is
	 * actually available: an explicit deviceId wins.
	 */
	constraints: Signal<Video.Constraints | undefined>;

	readonly #out: CameraOutput = {
		source: new Signal<Media | undefined>(undefined),
		orientation: new Signal(0),
		error: new Signal<Error | undefined>(undefined),
	};
	readonly out = readonlys(this.#out);

	#signals = new Effect();
	#retry = new Retry();

	constructor(props?: CameraProps) {
		this.in = {
			enabled: getter(props?.enabled ?? true),
		};
		this.device = new Device("video", props?.device);
		this.constraints = Signal.from(props?.constraints);

		this.#signals.run(this.#run.bind(this));
		this.#signals.run(this.#runOrientation.bind(this));
	}

	#runOrientation(effect: Effect): void {
		if (!effect.get(this.in.enabled)) return;

		const update = () => this.#out.orientation.set(screenOrientation());
		const orientation = globalThis.screen?.orientation;
		if (orientation) effect.event(orientation, "change", update);
		effect.event(globalThis, "orientationchange", update);
		effect.set(this.#out.orientation, screenOrientation(), 0);
	}

	#run(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		if (!enabled) {
			// Being switched off is the app's reset, so a later enable starts with a full budget.
			this.#retry.refund();
			this.#out.error.set(undefined);
			return;
		}

		// Read the settings before checking the budget, so changing one both reruns this effect and
		// buys it a fresh budget.
		const device = effect.get(this.device.out.requested);
		const constraints = effect.get(this.constraints);

		if (!this.#retry.begin(effect, [device, constraints])) {
			// `failure` is set only when nothing remains, so a backoff stays quiet.
			const failure = this.#retry.spent();
			if (failure) this.#out.error.set(failure);

			// Waiting out a backoff, or out of budget entirely, with the same settings. Either way
			// a change to what is plugged in is new information worth acting on now, so watch the
			// device list here and not while healthy, where a rerun would restart a working capture
			// for unrelated device churn.
			const spent = this.device.out.available.peek();
			effect.subscribe(this.device.out.available, (available) => {
				if (available !== spent) this.#retry.refund();
			});
			const permitted = this.device.out.permission.peek();
			effect.subscribe(this.device.out.permission, (granted) => {
				if (granted && !permitted) this.#retry.refund();
			});
			return;
		}

		this.#out.error.set(undefined);

		// Build final constraints with device selection, defaulting resolution unless overridden.
		const finalConstraints: MediaTrackConstraints = {
			...Camera.DEFAULT_CONSTRAINTS,
			...constraints,
			deviceId: device ? { exact: device } : undefined,
		};

		effect.spawn(async () => {
			const media = navigator.mediaDevices.getUserMedia({ video: finalConstraints });

			// If the effect is cancelled, stop any stream that arrives after cancellation too.
			effect.cleanup(() =>
				media.then(
					(stream) =>
						stream.getTracks().forEach((track) => {
							track.stop();
						}),
					() => {},
				),
			);

			let stream: MediaStream | undefined;
			try {
				stream = await effect.race(media);
			} catch (error) {
				if (effect.abort.aborted) return;
				if (this.#retry.rejected(error)) return;
				this.#out.error.set(error instanceof Error ? error : new Error(String(error)));
				return;
			}

			// A torn-down run is not a failed attempt: whatever cancelled it reruns us.
			if (effect.abort.aborted || !stream) return;

			const source = stream.getVideoTracks()[0] as Video.StreamTrack | undefined;

			// getUserMedia resolved, so we have permission even if no track came back.
			effect.cleanup(this.device.capture(source?.getSettings().deviceId));

			// A track that arrives dead already fired "ended", so nothing would ever rerun us.
			if (!source || source.readyState === "ended") {
				this.#retry.ended();
				return;
			}

			this.#retry.succeeded(effect, source);
			effect.set(this.#out.source, { video: source });
		});
	}

	/** Stop the capture and release the device. */
	close() {
		this.#signals.close();
		this.device.close();
	}
}

// Camera sensors deliver landscape pictures, so a portrait screen needs a quarter turn.
function screenOrientation(): number {
	const type = globalThis.screen?.orientation?.type;
	if (type === "portrait-primary") return 90;
	if (type === "portrait-secondary") return 270;
	if (type) return 0;

	// Safari historically exposed only Window.orientation. Its portrait values are 0 and 180.
	const angle = Reflect.get(globalThis, "orientation");
	if (typeof angle !== "number") return 0;
	const normalized = ((angle % 360) + 360) % 360;
	if (normalized === 0) return 90;
	if (normalized === 180) return 270;
	return 0;
}
