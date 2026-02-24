import { Effect, type Getter, Signal } from "@moq/signals";

export interface DeviceProps {
	preferred?: string | Signal<string | undefined>;
}

export class Device<Kind extends "audio" | "video"> {
	kind: Kind;

	// The devices that are available.
	#devices = new Signal<MediaDeviceInfo[] | undefined>(undefined);
	readonly available: Getter<MediaDeviceInfo[] | undefined> = this.#devices;

	// The default device based on heuristics.
	#default = new Signal<string | undefined>(undefined);
	readonly default: Getter<string | undefined> = this.#default;

	// The deviceId that we want to use, otherwise use the default device.
	preferred: Signal<string | undefined>;

	// The device that we are actually using.
	active = new Signal<string | undefined>(undefined);

	// Whether we have permission to enumerate devices.
	permission = new Signal<boolean>(false);

	// The device we want to use next. (preferred ?? default)
	#requested = new Signal<string | undefined>(undefined);
	requested: Getter<string | undefined> = this.#requested;

	signals = new Effect();

	constructor(kind: Kind, props?: DeviceProps) {
		this.kind = kind;
		this.preferred = Signal.from(props?.preferred);

		this.signals.run((effect) => {
			effect.spawn(this.#run.bind(this, effect));
			effect.event(navigator.mediaDevices, "devicechange", () => this.permission.mutate(() => {}));
		});

		this.signals.run(this.#runRequested.bind(this));
	}

	async #run(effect: Effect) {
		// Force a reload of the devices list if we don't have permission.
		// We still try anyway.
		effect.get(this.permission);

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
			this.#devices.set(undefined);
			this.#default.set(undefined);
			return;
		}

		// Assume we have permission now.
		this.permission.set(true);

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

		this.#devices.set(devices);
		this.#default.set(defaultDevice?.deviceId);
	}

	#runRequested(effect: Effect) {
		const preferred = effect.get(this.preferred);
		if (preferred && effect.get(this.#devices)?.some((d) => d.deviceId === preferred)) {
			// Use the preferred device if it's in our devices list.
			this.#requested.set(preferred);
		} else {
			// Otherwise use the default device.
			this.#requested.set(effect.get(this.default));
		}
	}

	// Manually request permission for the device, ignoring the result.
	requestPermission() {
		if (this.permission.peek()) return;

		navigator.mediaDevices
			.getUserMedia({ [this.kind]: true })
			.then((stream) => {
				this.permission.set(true);

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

	close() {
		this.signals.close();
	}
}
