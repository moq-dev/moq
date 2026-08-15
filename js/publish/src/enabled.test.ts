import { expect, mock, test } from "bun:test";

// Bun cannot load the blob-URL worker/worklet imports used by the media pipelines. Nothing in
// these default-input tests reaches either implementation.
mock.module("./video/capture-worker.ts?worker&inline", () => ({ default: class {} }));
mock.module("./audio/capture-worklet.ts?worklet", () => ({ default: "blob:fake-capture" }));

const Publish = await import("./index");

type Mode = "omitted" | boolean | undefined;

class FakeMediaDevices extends EventTarget {
	userMedia = 0;
	displayMedia = 0;

	async enumerateDevices(): Promise<MediaDeviceInfo[]> {
		return [device("camera", "videoinput"), device("microphone", "audioinput")];
	}

	getUserMedia(): Promise<MediaStream> {
		this.userMedia++;
		return new Promise(() => {});
	}

	getDisplayMedia(): Promise<MediaStream> {
		this.displayMedia++;
		return new Promise(() => {});
	}
}

function device(deviceId: string, kind: MediaDeviceKind): MediaDeviceInfo {
	return { deviceId, label: deviceId, groupId: deviceId, kind, toJSON: () => ({}) };
}

type InstalledMediaDevices = {
	media: FakeMediaDevices;
	restore(): void;
};

function installMediaDevices(): InstalledMediaDevices {
	const media = new FakeMediaDevices();
	const navigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");
	const self = Object.getOwnPropertyDescriptor(globalThis, "self");
	Object.defineProperty(globalThis, "navigator", {
		configurable: true,
		value: { mediaDevices: media, userAgent: "bun" },
	});
	Object.defineProperty(globalThis, "self", { configurable: true, value: globalThis });

	return {
		media,
		restore() {
			if (navigator) Object.defineProperty(globalThis, "navigator", navigator);
			else Reflect.deleteProperty(globalThis, "navigator");
			if (self) Object.defineProperty(globalThis, "self", self);
			else Reflect.deleteProperty(globalThis, "self");
		},
	};
}

function inputs(mode: Mode): { enabled?: boolean } | undefined {
	return mode === "omitted" ? undefined : { enabled: mode };
}

const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));
async function settle(times = 5): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

async function enabled(mode: Mode): Promise<{ values: boolean[]; userMedia: number; displayMedia: number }> {
	const installed = installMediaDevices();
	const props = inputs(mode);
	const components = [
		new Publish.Broadcast(props),
		new Publish.Video.Encoder("video", props),
		new Publish.Audio.Capture(props),
		new Publish.Audio.Encoder("audio", props),
		new Publish.Source.Camera(props),
		new Publish.Source.Microphone(props),
		new Publish.Source.Screen(props),
		new Publish.Source.File(props),
	];

	try {
		await settle();
		const values = components.map((component) => component.in.enabled.peek());
		return {
			values,
			userMedia: installed.media.userMedia,
			displayMedia: installed.media.displayMedia,
		};
	} finally {
		for (const component of components) component.close();
		await settle();
		installed.restore();
	}
}

test("standalone components default enabled", async () => {
	expect(await enabled("omitted")).toEqual({
		values: [true, true, true, true, true, true, true, true],
		userMedia: 2,
		displayMedia: 1,
	});
	expect(await enabled(undefined)).toEqual({
		values: [true, true, true, true, true, true, true, true],
		userMedia: 2,
		displayMedia: 1,
	});
	expect(await enabled(false)).toEqual({
		values: [false, false, false, false, false, false, false, false],
		userMedia: 0,
		displayMedia: 0,
	});
});
