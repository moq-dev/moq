import { expect, mock, test } from "bun:test";
import { Signal } from "@moq/signals";

// Bun does not run Vite's worklet loader; these tests never create an AudioContext.
mock.module("./audio/render-worklet.ts?worklet", () => ({ default: "blob:fake-render" }));

const { Player } = await import("./player");

async function flush() {
	for (let i = 0; i < 10; i++) await Promise.resolve();
}

test("Player owns the shared pipeline and feeds output policy into decoders", async () => {
	const enabled = new Signal(true);
	const paused = new Signal(false);
	const muted = new Signal(false);
	const player = new Player({ enabled, paused, muted, visible: "always" });
	try {
		await flush();
		expect(player.video.source.in.broadcast.peek()).toBe(player.broadcast);
		expect(player.audio.source.in.broadcast.peek()).toBe(player.broadcast);
		expect(player.text.in.broadcast.peek()).toBe(player.broadcast);
		expect(player.video.sync).toBe(player.sync);
		expect(player.audio.sync).toBe(player.sync);
		expect(player.renderer.decoder).toBe(player.video);
		expect(player.emitter.source).toBe(player.audio);
		expect(player.video.in.enabled.peek()).toBe(true);
		expect(player.audio.in.enabled.peek()).toBe(true);
		expect(player.textRenderer.in.enabled.peek()).toBe(true);

		muted.set(true);
		await flush();
		expect(player.audio.in.enabled.peek()).toBe(false);
		expect(player.video.in.enabled.peek()).toBe(true);

		paused.set(true);
		await flush();
		expect(player.textRenderer.in.enabled.peek()).toBe(false);
		// A paused player keeps one video frame as a poster.
		expect(player.video.in.enabled.peek()).toBe(true);

		enabled.set(false);
		await flush();
		expect(player.broadcast.in.enabled.peek()).toBe(false);
		expect(player.video.in.enabled.peek()).toBe(false);
		expect(player.audio.in.enabled.peek()).toBe(false);
		expect(player.textRenderer.in.enabled.peek()).toBe(false);
	} finally {
		player.close();
	}
});
