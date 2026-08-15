import { expect, mock, test } from "bun:test";
import * as Moq from "@moq/net";

// Bun cannot load the blob-URL worklet import used by the audio decoder. This test never provides
// an audio rendition, so the decoder never creates an AudioContext or reaches the worklet.
mock.module("../../watch/src/audio/render-worklet.ts?worklet", () => ({ default: "blob:fake-render" }));

const { Game } = await import("./game");

const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));
async function settle(times = 5): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

test("audio downloads only while the tile is active and audible", async () => {
	const originalDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
	Object.defineProperty(globalThis, "document", { configurable: true, value: new EventTarget() });

	const connection = new Moq.Connection.Reload({ enabled: false });
	const origin = new Moq.Origin.Producer();
	const game = new Game({
		sessionId: "test",
		connection,
		origin,
		expanded: new Moq.Signals.Signal<string | undefined>(undefined),
		gamePrefix: "game",
		viewerPrefix: "viewer",
	});

	try {
		expect(game.audioEmitter.out.enabled.peek()).toBe(false);
		expect(game.audioDecoder.in.enabled.peek()).toBe(false);

		game.hovered.set(true);
		await settle();
		expect(game.audioEmitter.out.enabled.peek()).toBe(true);
		expect(game.audioDecoder.in.enabled.peek()).toBe(true);

		game.userMuted.set(true);
		await settle();
		expect(game.audioEmitter.out.enabled.peek()).toBe(false);
		expect(game.audioDecoder.in.enabled.peek()).toBe(false);
	} finally {
		game.close();
		origin.close();
		connection.close();
		if (originalDocument) Object.defineProperty(globalThis, "document", originalDocument);
		else Reflect.deleteProperty(globalThis, "document");
	}
});
