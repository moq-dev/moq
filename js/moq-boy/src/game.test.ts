import { expect, mock, spyOn, test } from "bun:test";
import * as Moq from "@moq/net";

// Bun cannot load the blob-URL worklet import used by the audio decoder. This test never provides
// an audio rendition, so the decoder never creates an AudioContext or reaches the worklet.
mock.module("../../watch/src/audio/render-worklet.ts?worklet", () => ({ default: async () => "blob:fake-render" }));

const { Game } = await import("./game");

const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));
async function settle(times = 5): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

test("audio downloads only while the tile is active and audible", async () => {
	const originalDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
	Object.defineProperty(globalThis, "document", { configurable: true, value: new EventTarget() });

	const connection = new Moq.Connection({ enabled: false });
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

test("status skips values that fail the schema and ends when the track errors", async () => {
	const originalDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
	Object.defineProperty(globalThis, "document", { configurable: true, value: new EventTarget() });
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const error = spyOn(console, "error").mockImplementation(() => {});
	const refused = () => warn.mock.calls.filter(([msg]) => msg === "Invalid status:").length;

	const connection = new Moq.Connection({ enabled: false });
	const origin = new Moq.Origin.Producer();
	const publication = origin.route(Moq.Path.from("game", "test"));
	const track = publication.broadcast.createTrack("status");

	// One finished group per status, so between statuses the loop waits on the next group.
	const publish = (value: unknown) => {
		const group = track.appendGroup();
		group.writeFrame({ payload: new TextEncoder().encode(JSON.stringify(value)) });
		group.finish();
	};

	const game = new Game({
		sessionId: "test",
		connection,
		origin,
		expanded: new Moq.Signals.Signal<string | undefined>(undefined),
		gamePrefix: "game",
		viewerPrefix: "viewer",
	});

	try {
		// A value that fails the schema is refused and the loop keeps reading.
		publish({ buttons: "a" });
		await settle(30);
		expect(game.status.peek()).toBeUndefined();
		expect(refused()).toBe(1);

		const valid = { buttons: ["a"], latency: {} };
		publish(valid);
		await settle(30);
		expect(game.status.peek()).toEqual(valid);

		// A track closed with an error throws on every read, so the loop ends instead of spinning.
		track.abort(new Error("boom"));
		await settle(30);
		expect(refused()).toBe(1);
		expect(error.mock.calls.filter(([msg]) => msg === "spawn error")).toHaveLength(1);
	} finally {
		game.close();
		origin.close();
		connection.close();
		warn.mockRestore();
		error.mockRestore();
		if (originalDocument) Object.defineProperty(globalThis, "document", originalDocument);
		else Reflect.deleteProperty(globalThis, "document");
	}
});
