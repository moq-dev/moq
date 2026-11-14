import { expect, mock, spyOn, test } from "bun:test";
import * as Moq from "@moq/net";

// Bun cannot load the blob-URL worklet import used by the audio decoder. This test never provides
// an audio rendition, so the decoder never creates an AudioContext or reaches the worklet.
mock.module("../../../js/watch/src/audio/render-worklet.ts?worklet", () => ({
	default: async () => "blob:fake-render",
}));

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
	const endpoint = new Moq.Socket.Endpoint(origin, Moq.Path.from("game"), "viewer-test");
	const game = new Game({
		sessionId: "test",
		connection,
		origin,
		endpoint,
		expanded: new Moq.Signals.Signal<string | undefined>(undefined),
		gamePrefix: "game",
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
		endpoint.close();
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
		group.writeFrame({
			payload: new TextEncoder().encode(JSON.stringify(value)),
			timestamp: Moq.Time.Timestamp.unscaled(group.sequence),
		});
		group.finish();
	};

	const endpoint = new Moq.Socket.Endpoint(origin, Moq.Path.from("game"), "viewer-test");
	const game = new Game({
		sessionId: "test",
		connection,
		origin,
		endpoint,
		expanded: new Moq.Signals.Signal<string | undefined>(undefined),
		gamePrefix: "game",
	});

	try {
		// A value that fails the schema is refused and the loop keeps reading.
		publish({ buttons: "a" });
		await settle(30);
		expect(game.status.peek()).toBeUndefined();
		expect(refused()).toBe(1);

		const valid = { buttons: ["a"], players: 0 };
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
		endpoint.close();
		origin.close();
		connection.close();
		warn.mockRestore();
		error.mockRestore();
		if (originalDocument) Object.defineProperty(globalThis, "document", originalDocument);
		else Reflect.deleteProperty(globalThis, "document");
	}
});

test("input opens a session that carries commands up and latency down until deactivated", async () => {
	const originalDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
	Object.defineProperty(globalThis, "document", { configurable: true, value: new EventTarget() });

	const connection = new Moq.Connection({ enabled: false });
	// The session waits for a live connection; the endpoints share one origin instead of a relay.
	Object.defineProperty(connection, "status", { value: new Moq.Signals.Signal("connected") });
	const origin = new Moq.Origin.Producer();
	const emulator = new Moq.Socket.Endpoint(origin, Moq.Path.from("game"), "test");
	const endpoint = new Moq.Socket.Endpoint(origin, Moq.Path.from("game"), "viewer-test");
	const game = new Game({
		sessionId: "test",
		connection,
		origin,
		endpoint,
		expanded: new Moq.Signals.Signal<string | undefined>(undefined),
		gamePrefix: "game",
	});

	try {
		game.hovered.set(true);
		game.heldButtons.add("a");
		game.sendButtons();

		const incoming = await emulator.acceptTrack();
		expect(incoming.peer).toBe("viewer-test");
		const session = await incoming.accept();
		const commands = session.consumer.ordered();
		const command = await (await commands.nextGroup())?.readJson();
		expect(command).toEqual({ type: "buttons", buttons: ["a"], timestamps: [] });

		const latency = [{ label: "input", ms: 42 }];
		const reply = session.producer.appendGroup();
		reply.writeFrame({
			payload: new TextEncoder().encode(JSON.stringify({ latency })),
			timestamp: Moq.Time.Timestamp.unscaled(reply.sequence),
		});
		reply.finish();
		await settle(30);
		expect(game.latency.peek()).toEqual(latency);

		// Leaving the tile ends the session, so the emulator counts this viewer gone.
		game.hovered.set(false);
		expect(await commands.nextGroup()).toBeUndefined();
		await settle();
		expect(game.latency.peek()).toBeUndefined();
	} finally {
		game.close();
		endpoint.close();
		emulator.close();
		origin.close();
		connection.close();
		if (originalDocument) Object.defineProperty(globalThis, "document", originalDocument);
		else Reflect.deleteProperty(globalThis, "document");
	}
});

// Input given while offline would land late, out of context; a reset pressed then must not fire on reconnect.
test("a command sent while disconnected is dropped, not replayed on reconnect", async () => {
	const originalDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
	Object.defineProperty(globalThis, "document", { configurable: true, value: new EventTarget() });

	const connection = new Moq.Connection({ enabled: false });
	const status = new Moq.Signals.Signal<string>("connecting");
	Object.defineProperty(connection, "status", { value: status });
	const origin = new Moq.Origin.Producer();
	const emulator = new Moq.Socket.Endpoint(origin, Moq.Path.from("game"), "test");
	const endpoint = new Moq.Socket.Endpoint(origin, Moq.Path.from("game"), "viewer-test");
	const game = new Game({
		sessionId: "test",
		connection,
		origin,
		endpoint,
		expanded: new Moq.Signals.Signal<string | undefined>(undefined),
		gamePrefix: "game",
	});

	try {
		game.hovered.set(true);
		game.sendCommand({ type: "reset" });
		await settle();

		let dialed = false;
		const incoming = emulator.acceptTrack().then((track) => {
			dialed = true;
			return track;
		});
		status.set("connected");
		await settle(50);
		expect(dialed).toBe(false);

		game.heldButtons.add("a");
		game.sendButtons();
		const session = await (await incoming).accept();
		const commands = session.consumer.ordered();
		expect(await (await commands.nextGroup())?.readJson()).toEqual({
			type: "buttons",
			buttons: ["a"],
			timestamps: [],
		});
	} finally {
		game.close();
		endpoint.close();
		emulator.close();
		origin.close();
		connection.close();
		if (originalDocument) Object.defineProperty(globalThis, "document", originalDocument);
		else Reflect.deleteProperty(globalThis, "document");
	}
});
