import { expect, test } from "bun:test";
import { Effect, Signal } from "@moq/signals";
import type * as Audio from "./audio";
import type * as Source from "./source";
import { clearSourceState, type SourceState } from "./source-state";
import type * as Video from "./video";

const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));

async function settle(times = 5): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

test("switching and clearing selections removes every inactive source", async () => {
	const selected = new Signal<"camera" | "file" | undefined>("camera");
	const camera = { kind: "camera" } as unknown as Source.Camera;
	const microphone = { kind: "microphone" } as unknown as Source.Microphone;
	const file = { kind: "file" } as unknown as Source.File;
	const video = { kind: "video" } as unknown as Video.Source;
	const audio = { kind: "audio" } as unknown as Audio.Source;

	const state: SourceState = {
		holders: {
			video: new Signal<Source.Camera | Source.Screen | undefined>(undefined),
			audio: new Signal<Source.Microphone | Source.Screen | undefined>(undefined),
			file: new Signal<Source.File | undefined>(undefined),
		},
		video: new Signal<Video.Source | undefined>(undefined),
		audio: new Signal<Audio.Source | undefined>(undefined),
	};

	const effect = new Effect((effect) => {
		const source = effect.get(selected);
		clearSourceState(effect, state);

		if (source === "camera") {
			effect.set(state.holders.video, camera);
			effect.set(state.holders.audio, microphone);
			effect.set(state.video, video);
			effect.set(state.audio, audio);
		} else if (source === "file") {
			effect.set(state.holders.file, file);
		}
	});

	try {
		await settle();
		expect(state.holders.video.peek()).toBe(camera);
		expect(state.holders.audio.peek()).toBe(microphone);
		expect(state.video.peek()).toBe(video);
		expect(state.audio.peek()).toBe(audio);

		selected.set("file");
		await settle();
		expect(state.holders.video.peek()).toBeUndefined();
		expect(state.holders.audio.peek()).toBeUndefined();
		expect(state.holders.file.peek()).toBe(file);
		expect(state.video.peek()).toBeUndefined();
		expect(state.audio.peek()).toBeUndefined();

		selected.set(undefined);
		await settle();
		expect(state.holders.video.peek()).toBeUndefined();
		expect(state.holders.audio.peek()).toBeUndefined();
		expect(state.holders.file.peek()).toBeUndefined();
		expect(state.video.peek()).toBeUndefined();
		expect(state.audio.peek()).toBeUndefined();
	} finally {
		effect.close();
	}
});
