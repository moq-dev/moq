import { expect, mock, test } from "bun:test";
import { Signal } from "@moq/signals";
import { Screen } from "./screen";

const settle = async () => {
	for (let i = 0; i < 20; i++) await Promise.resolve();
};

class FakeTrack extends EventTarget {
	readyState: MediaStreamTrackState = "live";
	readonly stop = mock(() => {
		this.readyState = "ended";
	});
	getSettings() {
		return {};
	}
	// What the browser's own "Stop sharing" control does.
	end() {
		this.readyState = "ended";
		this.dispatchEvent(new Event("ended"));
	}
}

function stream(video = new FakeTrack(), audio = new FakeTrack()): MediaStream {
	return {
		getTracks: () => [video, audio],
		getVideoTracks: () => [video],
		getAudioTracks: () => [audio],
	} as unknown as MediaStream;
}

// Each call to getDisplayMedia opens a picker the test answers.
function install() {
	const pickers: PromiseWithResolvers<MediaStream>[] = [];
	const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");
	const originalSelf = Object.getOwnPropertyDescriptor(globalThis, "self");
	Object.defineProperty(globalThis, "navigator", {
		configurable: true,
		value: {
			mediaDevices: {
				getDisplayMedia: () => {
					const picker = Promise.withResolvers<MediaStream>();
					pickers.push(picker);
					return picker.promise;
				},
			},
		},
	});
	Object.defineProperty(globalThis, "self", { configurable: true, value: {} });
	return {
		pickers,
		[Symbol.dispose]() {
			if (originalNavigator) Object.defineProperty(globalThis, "navigator", originalNavigator);
			else Reflect.deleteProperty(globalThis, "navigator");
			if (originalSelf) Object.defineProperty(globalThis, "self", originalSelf);
			else Reflect.deleteProperty(globalThis, "self");
		},
	};
}

for (const cancel of ["close", "disable"] as const) {
	test(`a screen picked after ${cancel} releases every track`, async () => {
		using env = install();
		const enabled = new Signal(true);
		const screen = new Screen({ enabled });
		const tracks = [new FakeTrack(), new FakeTrack()];
		try {
			await settle();
			if (cancel === "close") screen.close();
			else enabled.set(false);
			await settle();
			env.pickers[0]?.resolve(stream(...tracks));
			await settle();
			for (const track of tracks) expect(track.stop).toHaveBeenCalledTimes(1);
			expect(screen.out.source.peek()).toBeUndefined();
			expect(screen.out.state.peek()).toEqual({ kind: "off" });
		} finally {
			screen.close();
		}
	});
}

// A rejected picker is over, not still prompting, and nothing but a new enable prompts again.
for (const [name, reason] of [
	["NotAllowedError", "dismissed"],
	["NotFoundError", "error"],
] as const) {
	test(`a ${name} from the picker ends the share as ${reason} without prompting again`, async () => {
		using env = install();
		const enabled = new Signal(true);
		const screen = new Screen({ enabled });
		try {
			await settle();
			expect(screen.out.state.peek()).toEqual({ kind: "prompting" });

			const error = new DOMException("no", name);
			env.pickers[0]?.reject(error);
			await settle();
			expect(screen.out.state.peek()).toEqual(
				reason === "error" ? { kind: "ended", reason, error } : { kind: "ended", reason },
			);
			expect(screen.out.source.peek()).toBeUndefined();

			screen.video.set({ frameRate: 15 });
			screen.audio.set(false);
			await settle();
			expect(env.pickers).toHaveLength(1);

			// Switching off keeps the reason; switching back on is the gesture that prompts again.
			enabled.set(false);
			await settle();
			expect(screen.out.state.peek().kind).toBe("ended");
			enabled.set(true);
			await settle();
			expect(env.pickers).toHaveLength(2);
			expect(screen.out.state.peek()).toEqual({ kind: "prompting" });
		} finally {
			screen.close();
		}
	});
}

test("a share the user stops releases both tracks and stays ended", async () => {
	using env = install();
	const screen = new Screen();
	const video = new FakeTrack();
	const audio = new FakeTrack();
	try {
		await settle();
		env.pickers[0]?.resolve(stream(video, audio));
		await settle();
		const state = screen.out.state.peek();
		expect(state.kind).toBe("live");
		expect(screen.out.state.peek()).toMatchObject({ kind: "live", media: { video: { track: video } } });

		video.end();
		await settle();
		expect(screen.out.state.peek()).toEqual({ kind: "ended", reason: "stopped" });
		expect(screen.out.source.peek()).toBeUndefined();
		expect(audio.stop).toHaveBeenCalled();

		screen.video.set({ frameRate: 15 });
		await settle();
		expect(env.pickers).toHaveLength(1);
	} finally {
		screen.close();
	}
});

test("a share picked dead is stopped, not live", async () => {
	using env = install();
	const screen = new Screen();
	const video = new FakeTrack();
	video.readyState = "ended";
	try {
		await settle();
		env.pickers[0]?.resolve(stream(video));
		await settle();
		expect(screen.out.state.peek()).toEqual({ kind: "ended", reason: "stopped" });
		expect(screen.out.source.peek()).toBeUndefined();
	} finally {
		screen.close();
	}
});

// The constraints are read when the picker opens, so editing them never prompts on its own.
test("changing constraints while live does not prompt", async () => {
	using env = install();
	const screen = new Screen({ video: { frameRate: 30 } });
	try {
		await settle();
		env.pickers[0]?.resolve(stream());
		await settle();
		screen.video.set({ frameRate: 15 });
		await settle();
		expect(env.pickers).toHaveLength(1);
		expect(screen.out.state.peek().kind).toBe("live");
	} finally {
		screen.close();
	}
});
