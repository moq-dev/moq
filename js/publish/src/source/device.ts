import { type Dispose, Effect, readonlys, Signal } from "@moq/signals";

/** Constructor options for {@link Device}. */
export interface DeviceProps {
	/** Seed the preferred device; also live-editable via `device.preferred`. */
	preferred?: string | Signal<string | undefined>;
}

type DeviceOutput = {
	// The devices that are available, or undefined without permission to enumerate them.
	available: Signal<MediaDeviceInfo[] | undefined>;
	// The default device based on heuristics.
	default: Signal<string | undefined>;
	// The device we want to use next. (preferred ?? default)
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
 * capture via {@link capture}. Set {@link preferred} to choose one; the rest is discovered.
 */
export class Device<Kind extends "audio" | "video"> {
	/** Whether this tracks audio inputs or video inputs. */
	readonly kind: Kind;

	/** The deviceId to use, or undefined to use the default. */
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

		let defaultDevice: MediaDeviceInfo | undefined;
		if (alias) {
			// Find the device with the same groupId as the default alias.
			defaultDevice = devices.find((d) => d.groupId === alias.groupId);
		}

		// If we couldn't find a default alias, time to scan labels.
		if (!defaultDevice) {
			if (this.kind === "audio") {
				// Look for default or communications device
				defaultDevice = devices.find((d) => {
					const label = d.label.toLowerCase();
					return label.includes("default") || label.includes("communications");
				});
			} else if (this.kind === "video") {
				// On mobile, prefer front-facing camera
				defaultDevice = devices.find((d) => {
					const label = d.label.toLowerCase();
					return label.includes("front") || label.includes("external") || label.includes("usb");
				});
			}
		}

		if (!defaultDevice) {
			// Still nothing, then use the top one.
			defaultDevice = devices.at(0);
		}

		this.#out.available.set(devices);
		this.#out.default.set(defaultDevice?.deviceId);
	}

	#runRequested(effect: Effect) {
		const preferred = effect.get(this.preferred);
		if (preferred && effect.get(this.out.available)?.some((d) => d.deviceId === preferred)) {
			// Use the preferred device if it's in our devices list.
			this.#out.requested.set(preferred);
		} else {
			// Otherwise use the default device.
			this.#out.requested.set(effect.get(this.out.default));
		}
	}

	/** Manually request permission for the device, ignoring the result. */
	requestPermission() {
		if (this.out.permission.peek()) return;

		navigator.mediaDevices
			.getUserMedia({ [this.kind]: true })
			.then((stream) => {
				this.#out.permission.set(true);

				// If the user selected a device during the dialog prompt, save it as the preferred device.
				const deviceId = stream.getTracks().at(0)?.getSettings().deviceId;
				if (deviceId) {
					this.preferred.set(deviceId);
				}

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
