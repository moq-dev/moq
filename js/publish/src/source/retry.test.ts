import { expect, test } from "bun:test";
import { Signal } from "@moq/signals";
import { Camera } from "./camera";
import { Microphone } from "./microphone";
import { Retry } from "./retry";
import { Screen } from "./screen";

// A MediaStreamTrack ends on its own when the device disappears or the OS revokes it. Only the bits
// the sources touch, plus end() to fire it.
class FakeTrack extends EventTarget {
	readyState: MediaStreamTrackState = "live";
	stopped = false;

	getSettings(): MediaTrackSettings {
		return { deviceId: "default" };
	}

	stop(): void {
		this.stopped = true;
		this.readyState = "ended";
	}

	/** Die the way an unplugged device does. */
	end(): void {
		this.readyState = "ended";
		this.dispatchEvent(new Event("ended"));
	}
}

class FakeMediaDevices extends EventTarget {
	tracks: FakeTrack[] = [];

	// getUserMedia rejects, the way it does once the last device is gone.
	missing = false;

	// Hand back an already-dead track, the way a device that vanished mid-call would.
	bornDead = false;

	// What enumerateDevices reports; replacing it and firing devicechange simulates a replug.
	devices: MediaDeviceInfo[] = [device("cam")];

	async enumerateDevices(): Promise<MediaDeviceInfo[]> {
		return this.devices;
	}

	// Every capture attempt, including the ones that never produce a track. `tracks` can't stand in:
	// a missing device rejects without pushing one.
	attempts = 0;

	async getUserMedia(): Promise<MediaStream> {
		this.attempts += 1;
		if (this.missing) throw new Error("NotFoundError");

		const track = new FakeTrack();
		if (this.bornDead) track.readyState = "ended";
		this.tracks.push(track);

		return {
			getTracks: () => [track],
			getAudioTracks: () => [track],
			getVideoTracks: () => [track],
		} as unknown as MediaStream;
	}

	/** The track handed to the most recent capture. */
	latest(): FakeTrack {
		const track = this.tracks.at(-1);
		if (!track) throw new Error("no track captured");
		return track;
	}

	/** Swap the device list and announce it, the way a replug does. */
	replug(devices: MediaDeviceInfo[]): void {
		this.devices = devices;
		this.dispatchEvent(new Event("devicechange"));
	}
}

// Screen capture hands back independent video and audio tracks, which can end separately.
class FakeScreenDevices extends EventTarget {
	readonly video = new FakeTrack();
	readonly audio = new FakeTrack();
	displays = 0;

	constructor(bornDead = false) {
		super();
		if (bornDead) this.video.readyState = "ended";
	}

	async getDisplayMedia(): Promise<MediaStream> {
		this.displays += 1;
		return {
			getTracks: () => [this.video, this.audio],
			getVideoTracks: () => [this.video],
			getAudioTracks: () => [this.audio],
		} as unknown as MediaStream;
	}
}

function device(deviceId: string): MediaDeviceInfo {
	return { deviceId, label: deviceId, groupId: deviceId, kind: "videoinput", toJSON: () => ({}) };
}

function install<T extends EventTarget>(media: T): T {
	Object.defineProperty(globalThis, "navigator", { configurable: true, value: { mediaDevices: media } });

	// Screen capture probes self.CaptureController.
	Object.defineProperty(globalThis, "self", { configurable: true, value: globalThis });
	return media;
}

const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));
async function settle(times = 20): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

/**
 * Poll until `pred` holds, so a regression fails the test instead of hanging it.
 *
 * A reopen waits out {@link Retry.DELAY} on a real timer, so microtask flushing alone never
 * reaches it. Polling rather than sleeping for the exact delay keeps a loaded runner from
 * deciding the outcome.
 */
async function waitUntil(pred: () => boolean): Promise<void> {
	const deadline = Date.now() + 10000;
	while (!pred()) {
		if (Date.now() > deadline) throw new Error("timed out waiting for condition");
		await new Promise((resolve) => setTimeout(resolve, 5));
	}
}

/**
 * Poll until the capture stops reopening, meaning the budget is spent.
 *
 * Watches for quiet rather than an exact attempt count: a device-list change can refund the budget
 * mid-cascade and buy another round. The window outlasts the largest backoff, so quiet here means
 * stopped rather than mid-wait.
 */
async function waitSpent(media: FakeMediaDevices): Promise<void> {
	const quiet = (Retry.DELAY.max ?? 0) + 100;

	let seen = -1;
	while (seen !== media.attempts) {
		seen = media.attempts;
		await new Promise((resolve) => setTimeout(resolve, quiet));
	}
	await settle();
}

// Burning the whole budget waits out every backoff, which outlasts the default per-test timeout.
const SPENT_TIMEOUT = 30000;

/** The track a source published, or undefined. */
function published(source: { track: MediaStreamTrack } | MediaStreamTrack | undefined): unknown {
	if (!source) return undefined;
	return "track" in source ? source.track : source;
}

test("a microphone re-opens when its track dies", async () => {
	const media = install(new FakeMediaDevices());

	const mic = new Microphone({ enabled: true });
	await settle();
	expect(media.tracks).toHaveLength(1);

	media.latest().end();
	await waitUntil(() => media.tracks.length === 2);
	await settle();

	// A second capture happened, and the live replacement is what got published.
	expect(published(mic.out.source.peek())).toBe(media.latest());

	mic.close();
});

test("a camera re-opens when its track dies", async () => {
	const media = install(new FakeMediaDevices());

	const camera = new Camera({ enabled: true });
	await settle();
	expect(media.tracks).toHaveLength(1);

	media.latest().end();
	await waitUntil(() => media.tracks.length === 2);
	await settle();

	expect(published(camera.out.source.peek())).toBe(media.latest());

	camera.close();
});

test("running out of retries clears the source instead of publishing a corpse", async () => {
	const media = install(new FakeMediaDevices());

	const mic = new Microphone({ enabled: true });
	await settle();

	// Kill every replacement the moment it arrives; the budget allows one reopen per failure.
	for (let i = 0; i < Retry.LIMIT; i++) {
		const before = media.tracks.length;
		media.latest().end();
		await waitUntil(() => media.tracks.length > before);
	}

	// The budget is spent, so this death schedules nothing at all: a settle is enough to catch a
	// reopen that shouldn't happen.
	media.latest().end();
	await settle();

	// The first capture plus one retry per allowed failure, and nothing after.
	expect(media.tracks).toHaveLength(Retry.LIMIT + 1);

	// Critically, the dead track is not left published: encoders would get no frames while the UI
	// still reported a live source.
	expect(mic.out.source.peek()).toBeUndefined();

	mic.close();
});

test("a track that arrives dead is not published", async () => {
	const media = install(new FakeMediaDevices());
	media.bornDead = true;

	const mic = new Microphone({ enabled: true });
	await settle();

	expect(mic.out.source.peek()).toBeUndefined();

	mic.close();
});

test(
	"a replug recovers a capture whose retries all failed",
	async () => {
		const media = install(new FakeMediaDevices());

		const camera = new Camera({ enabled: true });
		await settle();
		expect(media.tracks).toHaveLength(1);

		// Unplug the only camera: the track dies and every reopen finds nothing.
		media.missing = true;
		media.replug([]);
		media.latest().end();
		await waitSpent(media);

		const spent = media.tracks.length;
		expect(camera.out.source.peek()).toBeUndefined();

		// Plug it back in. Without the device-list refund this stays dead forever, because an unpinned
		// `requested` is undefined both before and after, so nothing else reruns the capture.
		media.missing = false;
		media.replug([device("cam")]);
		await waitUntil(() => media.tracks.length > spent);
		await settle();

		expect(media.tracks.length).toBeGreaterThan(spent);
		expect(published(camera.out.source.peek())).toBe(media.latest());

		camera.close();
	},
	SPENT_TIMEOUT,
);

test(
	"picking a different device revives a capture whose retries all failed",
	async () => {
		const media = install(new FakeMediaDevices());
		media.devices = [device("cam"), device("cam2")];

		const camera = new Camera({ enabled: true });
		await settle();

		// Burn the budget: every attempt hands back a dead track.
		media.bornDead = true;
		camera.device.preferred.set("cam");
		await waitSpent(media);

		const spent = media.tracks.length;
		expect(camera.out.source.peek()).toBeUndefined();

		// Selecting another device is the user's obvious recovery, and the device list has not changed,
		// so nothing else would rerun the capture.
		media.bornDead = false;
		camera.device.preferred.set("cam2");
		await waitUntil(() => media.tracks.length > spent);
		await settle();

		expect(media.tracks.length).toBeGreaterThan(spent);
		expect(published(camera.out.source.peek())).toBe(media.latest());

		camera.close();
	},
	SPENT_TIMEOUT,
);

test(
	"fixing a constraint revives a capture whose retries all failed",
	async () => {
		const media = install(new FakeMediaDevices());

		const mic = new Microphone({ enabled: true, constraints: { channelCount: 99 } });
		await settle();

		// An impossible constraint fails instantly on every attempt, which is the quickest way to spend
		// the whole budget.
		media.missing = true;
		mic.constraints.set({ channelCount: 98 });
		await waitSpent(media);

		const spent = media.tracks.length;
		expect(mic.out.source.peek()).toBeUndefined();

		media.missing = false;
		mic.constraints.set({ channelCount: 1 });
		await waitUntil(() => media.tracks.length > spent);
		await settle();

		expect(media.tracks.length).toBeGreaterThan(spent);
		expect(published(mic.out.source.peek())).toBe(media.latest());

		mic.close();
	},
	SPENT_TIMEOUT,
);

test("unrelated device churn does not disturb a healthy capture", async () => {
	const media = install(new FakeMediaDevices());

	const camera = new Camera({ enabled: true });
	await settle();
	expect(media.tracks).toHaveLength(1);

	// A second camera appears. Re-opening here would drop frames for no reason.
	media.replug([device("cam"), device("cam2")]);
	await settle(40);

	expect(media.tracks).toHaveLength(1);
	expect(published(camera.out.source.peek())).toBe(media.latest());

	camera.close();
});

test("screen capture releases every track when the video ends", async () => {
	const media = install(new FakeScreenDevices());

	const screen = new Screen({ enabled: true });
	await settle();
	expect(media.displays).toBe(1);
	expect(screen.out.source.peek()).toBeDefined();

	// Only the video track ends, as it does when the shared window closes. The audio track has to be
	// released too, or it keeps capturing invisibly.
	media.video.end();
	await settle();

	expect(screen.out.source.peek()).toBeUndefined();
	expect(media.audio.stopped).toBe(true);

	// getDisplayMedia needs a user gesture, so it must not re-prompt.
	expect(media.displays).toBe(1);

	screen.close();
});

test("screen capture releases every track when the audio ends", async () => {
	const media = install(new FakeScreenDevices());

	const screen = new Screen({ enabled: true });
	await settle();

	media.audio.end();
	await settle();

	expect(screen.out.source.peek()).toBeUndefined();
	expect(media.video.stopped).toBe(true);
	expect(media.displays).toBe(1);

	screen.close();
});

test("screen capture releases a share whose track arrives dead", async () => {
	const media = install(new FakeScreenDevices(true));

	const screen = new Screen({ enabled: true });
	await settle();

	// It fired "ended" before anything could listen, so publishing it would strand a dead share that
	// nothing clears.
	expect(screen.out.source.peek()).toBeUndefined();
	expect(media.audio.stopped).toBe(true);
	expect(media.displays).toBe(1);

	screen.close();
});

test("screen capture prompts again after being switched off and on", async () => {
	const media = install(new FakeScreenDevices());
	const enabled = new Signal(true);

	const screen = new Screen({ enabled });
	await settle();
	media.video.end();
	await settle();
	expect(media.displays).toBe(1);

	// Toggling the source off is the app's reset, so enabling it again is a fresh user action.
	enabled.set(false);
	await settle();
	enabled.set(true);
	await settle();

	expect(media.displays).toBe(2);

	screen.close();
});
