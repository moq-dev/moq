import { type Dispose, Effect, readonlys, Signal } from "@moq/signals";

/** Constructor options for {@link Device}. */
export interface DeviceProps {
	/** Seed the preferred device; also live-editable via `device.preferred`. */
	preferred?: string | Signal<string | undefined>;
}

type DeviceOutput = {
	// The devices that are available, or undefined without permission to enumerate them.
	available: Signal<MediaDeviceInfo[] | undefined>;
	// The device the platform names as its default, or undefined when it names none.
	// Informational only: it never pins a capture.
	default: Signal<string | undefined>;
	// The device to pin the next capture to, or undefined to let the browser choose.
	requested: Signal<string | undefined>;
	// The device backing the live capture, as reported by the owner via capture().
	active: Signal<string | undefined>;
	// Whether we have permission to enumerate devices.
	permission: Signal<boolean>;
};

/**
 * The available capture devices of one {@link kind}, and which of them to use.
 *
 * Owned by a source ({@link Camera}, {@link Microphone}), which reports the device backing its live
 * capture via {@link capture}. Set {@link preferred} to choose one; leave it unset and the capture
 * omits the `deviceId` constraint, following the browser and OS.
 */
export class Device<Kind extends "audio" | "video"> {
	/** Whether this tracks audio inputs or video inputs. */
	readonly kind: Kind;

	/** The deviceId to capture from, or undefined to let the browser and OS choose. Only the app writes this. */
	preferred: Signal<string | undefined>;

	readonly #out: DeviceOutput = {
		available: new Signal<MediaDeviceInfo[] | undefined>(undefined),
		default: new Signal<string | undefined>(undefined),
		requested: new Signal<string | undefined>(undefined),
		active: new Signal<string | undefined>(undefined),
		permission: new Signal<boolean>(false),
	};
	readonly out = readonlys(this.#out);

	#signals = new Effect();

	constructor(kind: Kind, props?: DeviceProps) {
		this.kind = kind;
		this.preferred = Signal.from(props?.preferred);

		this.#signals.run((effect) => {
			effect.spawn(this.#run.bind(this, effect));
			effect.event(navigator.mediaDevices, "devicechange", () => this.#out.permission.mutate(() => {}));
		});

		this.#signals.run(this.#runRequested.bind(this));
	}

	/**
	 * Report the device backing a live capture, granting permission as a side effect.
	 *
	 * Call it with the deviceId once `getUserMedia` succeeds, even if no track came back (the grant
	 * still happened). Dispose the returned handle when the capture stops to clear `out.active`.
	 */
	capture(deviceId: string | undefined): Dispose {
		this.#out.permission.set(true);
		this.#out.active.set(deviceId);

		return () => {
			if (this.#out.active.peek() === deviceId) this.#out.active.set(undefined);
		};
	}

	async #run(effect: Effect) {
		// Force a reload of the devices list if we don't have permission.
		// We still try anyway.
		effect.get(this.out.permission);

		// Ignore permission errors for now.
		let devices = await Promise.race([
			navigator.mediaDevices.enumerateDevices().catch(() => undefined),
			effect.cancel,
		]);
		if (!devices) return; // cancelled, keep stale values

		devices = devices.filter((d) => d.kind === `${this.kind}input`);

		// An empty deviceId means no permissions, or at the very least, no useful information.
		if (devices.some((d) => d.deviceId === "")) {
			console.warn(`no ${this.kind} permission`);
			this.#out.available.set(undefined);
			this.#out.default.set(undefined);
			return;
		}

		// Assume we have permission now.
		this.#out.permission.set(true);

		// No devices found, but we have permission I think?
		if (!devices.length) {
			console.warn(`no ${this.kind} devices found`);
		}

		// Chrome seems to have a "default" deviceId that we also need to filter out, but can be used to help us find the default device.
		const alias = devices.find((d) => d.deviceId === "default");

		// Remove the default device from the list.
		devices = devices.filter((d) => d.deviceId !== "default");

		// Only the alias resolves a default. Guessing by label or position is unsound: Android
		// enumerates output routes ("Headset earpiece", "Speakerphone") as audioinput devices, so
		// the first entry is a speaker rather than a microphone.
		const defaultDevice = alias ? devices.find((d) => d.groupId === alias.groupId) : undefined;

		this.#out.available.set(devices);
		this.#out.default.set(defaultDevice?.deviceId);
	}

	#runRequested(effect: Effect) {
		const preferred = effect.get(this.preferred);

		// Pin only a device that was asked for and is still present. Without a preference the
		// capture omits the constraint, which is the only way to follow the system default: a
		// deviceId we picked ourselves would override it forever.
		const known = effect.get(this.out.available)?.some((d) => d.deviceId === preferred) ?? false;
		this.#out.requested.set(preferred && known ? preferred : undefined);
	}

	/** Manually request permission for the device, ignoring the result. */
	requestPermission() {
		if (this.out.permission.peek()) return;

		navigator.mediaDevices
			.getUserMedia({ [this.kind]: true })
			.then((stream) => {
				this.#out.permission.set(true);

				stream.getTracks().forEach((track) => {
					track.stop();
				});
			})
			.catch(() => undefined);
	}

	/** Stop discovering devices. */
	close() {
		this.#signals.close();
	}
}
