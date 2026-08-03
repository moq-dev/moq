import { expect, test } from "bun:test";
import { Device } from "./device";
import { Microphone } from "./microphone";

// Android's Chromium enumerates communication devices (output routes) as audioinput, and lists the
// earpiece first. The "default" alias carries a groupId of its own, matching nothing.
const ANDROID: MediaDeviceInfo[] = [
	info("default", "Default", "a6c0aec8"),
	info("a38808f8b6", "Headset earpiece", "187ba5af"),
	info("135f4a3918", "Speakerphone", "b09d1d01"),
];

// Desktop Chrome: the alias shares a groupId with the device it points at.
const DESKTOP: MediaDeviceInfo[] = [
	info("default", "Default - Studio Mic", "group-a"),
	info("aaa", "Studio Mic", "group-a"),
	info("bbb", "Webcam Mic", "group-b"),
];

function info(deviceId: string, label: string, groupId: string): MediaDeviceInfo {
	return { deviceId, label, groupId, kind: "audioinput", toJSON: () => ({}) };
}

// Enough of MediaDevices for Device and Microphone, recording what getUserMedia was asked for.
class FakeMediaDevices extends EventTarget {
	requests: MediaStreamConstraints[] = [];
	readonly devices: MediaDeviceInfo[];
	readonly settled: string;

	constructor(devices: MediaDeviceInfo[], settled = "default") {
		super();
		this.devices = devices;
		this.settled = settled;
	}

	async enumerateDevices(): Promise<MediaDeviceInfo[]> {
		return this.devices;
	}

	async getUserMedia(constraints: MediaStreamConstraints): Promise<MediaStream> {
		this.requests.push(constraints);

		// An EventTarget, since the source watches the track for "ended".
		const track = Object.assign(new EventTarget(), {
			readyState: "live",
			getSettings: () => ({ deviceId: this.settled }),
			stop: () => {},
		});

		return { getTracks: () => [track], getAudioTracks: () => [track] } as unknown as MediaStream;
	}
}

function install(devices: MediaDeviceInfo[], settled?: string): FakeMediaDevices {
	const fake = new FakeMediaDevices(devices, settled);
	Object.defineProperty(globalThis, "navigator", {
		configurable: true,
		value: { mediaDevices: fake },
	});
	return fake;
}

const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));
async function settle(times = 10): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

test("an unresolvable default pins nothing", async () => {
	install(ANDROID);

	const device = new Device("audio");
	await settle();

	// The alias is dropped from the list but resolves no device, so there is nothing to pin.
	expect(device.out.available.peek()?.map((d) => d.label)).toEqual(["Headset earpiece", "Speakerphone"]);
	expect(device.out.default.peek()).toBeUndefined();
	expect(device.out.requested.peek()).toBeUndefined();

	device.close();
});

test("the platform default is reported but never pinned", async () => {
	install(DESKTOP);

	const device = new Device("audio");
	await settle();

	expect(device.out.default.peek()).toBe("aaa");
	expect(device.out.requested.peek()).toBeUndefined();

	device.close();
});

test("a preferred device is pinned once it is known to exist", async () => {
	install(DESKTOP);

	const device = new Device("audio", { preferred: "bbb" });
	await settle();
	expect(device.out.requested.peek()).toBe("bbb");

	// A device that isn't there is not pinned, rather than failing every capture.
	device.preferred.set("ccc");
	await settle();
	expect(device.out.requested.peek()).toBeUndefined();

	device.close();
});

test("a capture without a preference stays unpinned", async () => {
	const media = install(ANDROID);

	const mic = new Microphone({ enabled: true });
	await settle();

	// No deviceId constraint, so the browser and OS keep the routing decision.
	expect(media.requests).toHaveLength(1);
	const audio = media.requests[0]?.audio as MediaTrackConstraints | undefined;
	expect(audio?.deviceId).toBeUndefined();

	// The settled device is reported as active, but must not become a preference: that would pin
	// every later capture to a device the app never chose.
	expect(mic.device.out.active.peek()).toBe("default");
	expect(mic.device.preferred.peek()).toBeUndefined();
	expect(mic.device.out.requested.peek()).toBeUndefined();

	mic.close();
});
