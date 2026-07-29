import { expect, test } from "bun:test";
import { Camera } from "./camera";
import { Microphone } from "./microphone";
import { Restart } from "./restart";
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
	}

	/** Die the way an unplugged device does. */
	end(): void {
		this.readyState = "ended";
		this.dispatchEvent(new Event("ended"));
	}
}

class FakeMediaDevices extends EventTarget {
	tracks: FakeTrack[] = [];
	displays = 0;

	// Hand back an already-dead track, the way a device that vanished mid-call would.
	bornDead = false;

	async enumerateDevices(): Promise<MediaDeviceInfo[]> {
		return [{ deviceId: "mic", label: "Mic", groupId: "g", kind: "audioinput", toJSON: () => ({}) }];
	}

	async getUserMedia(): Promise<MediaStream> {
		return this.#stream();
	}

	async getDisplayMedia(): Promise<MediaStream> {
		this.displays += 1;
		return this.#stream();
	}

	/** The track handed to the most recent capture. */
	latest(): FakeTrack {
		const track = this.tracks.at(-1);
		if (!track) throw new Error("no track captured");
		return track;
	}

	#stream(): MediaStream {
		const track = new FakeTrack();
		if (this.bornDead) track.readyState = "ended";
		this.tracks.push(track);

		return {
			getTracks: () => [track],
			getAudioTracks: () => [track],
			getVideoTracks: () => [track],
		} as unknown as MediaStream;
	}
}

function install(): FakeMediaDevices {
	const fake = new FakeMediaDevices();
	Object.defineProperty(globalThis, "navigator", { configurable: true, value: { mediaDevices: fake } });

	// Screen capture probes self.CaptureController.
	Object.defineProperty(globalThis, "self", { configurable: true, value: globalThis });
	return fake;
}

const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));
async function settle(times = 10): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

test("a microphone re-opens when its track dies", async () => {
	const media = install();

	const mic = new Microphone({ enabled: true });
	await settle();
	expect(media.tracks).toHaveLength(1);

	media.latest().end();
	await settle();

	// A second capture happened, and the live replacement is what got published.
	expect(media.tracks).toHaveLength(2);
	const published = mic.out.source.peek();
	const track: unknown = published && "track" in published ? published.track : undefined;
	expect(track).toBe(media.latest());

	mic.close();
});

test("a camera re-opens when its track dies", async () => {
	const media = install();

	const camera = new Camera({ enabled: true });
	await settle();
	expect(media.tracks).toHaveLength(1);

	media.latest().end();
	await settle();

	expect(media.tracks).toHaveLength(2);

	camera.close();
});

test("a track that keeps dying stops being re-opened", async () => {
	const media = install();

	const mic = new Microphone({ enabled: true });
	await settle();

	// Kill every replacement the moment it arrives. Without a limit this spins forever.
	for (let i = 0; i < Restart.LIMIT + 5; i++) {
		media.latest().end();
		await settle();
	}

	// The first capture plus one retry per allowed failure, and nothing after.
	expect(media.tracks).toHaveLength(Restart.LIMIT + 1);

	mic.close();
});

test("a track that arrives dead is not published", async () => {
	const media = install();
	media.bornDead = true;

	const mic = new Microphone({ enabled: true });
	await settle();

	// It already fired "ended" before we could listen, so publishing it would strand a silent
	// capture that nothing retries.
	expect(media.tracks).toHaveLength(1);
	expect(mic.out.source.peek()).toBeUndefined();

	mic.close();
});

test("screen capture clears its source instead of re-opening", async () => {
	const media = install();

	const screen = new Screen({ enabled: true });
	await settle();
	expect(media.displays).toBe(1);
	expect(screen.out.source.peek()).toBeDefined();

	// getDisplayMedia needs a user gesture, and "Stop sharing" is an explicit stop, so the source
	// clears and the app decides what to do next.
	media.latest().end();
	await settle();

	expect(screen.out.source.peek()).toBeUndefined();
	expect(media.displays).toBe(1);

	screen.close();
});
