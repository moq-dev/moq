import { expect, mock, spyOn, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import * as Json from "@moq/json";
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
	if (!track) throw new Error("the track already exists");

	// One finished group per status, so between statuses the loop waits on the next group.
	const publish = (value: unknown) => {
		const group = track.appendGroup(Moq.Time.Timestamp.unscaled((track.largest()?.group ?? -1n) + 1n));
		group.writeFrame({
			payload: new TextEncoder().encode(JSON.stringify(value)),
			timestamp: Moq.Time.Timestamp.unscaled(group.start),
		});
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

		const valid = { buttons: ["a"], latency: { abc123: [{ label: "input", ms: 42 }] } };
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

// The viewer's command broadcast at {viewerPrefix}/{game}/{viewerId}, read as the emulator does.
async function commands(origin: Moq.Origin.Producer, viewerId: string) {
	const request = origin.consume().request(Moq.Path.from("viewer", "test", viewerId));
	await settle();
	const broadcast = request.active.peek();
	if (!broadcast) throw new Error("no command broadcast");
	return new Json.Snapshot.Consumer<Record<string, unknown>>({ track: broadcast.track("command") });
}

test("input publishes a command broadcast until the idle timer or deactivation closes it", async () => {
	const originalDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
	Object.defineProperty(globalThis, "document", { configurable: true, value: new EventTarget() });
	const clock = new Manual();
	const installed = clock.install();

	const connection = new Moq.Connection({ enabled: false });
	// The broadcast waits for a live connection; the emulator reads the same origin instead of a relay.
	Object.defineProperty(connection, "status", { value: new Moq.Signals.Signal("connected") });
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
		game.hovered.set(true);
		game.heldButtons.add("a");
		game.sendButtons();
		await settle();

		const first = game.viewerId.peek();
		expect(first).toHaveLength(6);
		const reader = await commands(origin, first as string);
		expect((await reader.next())?.value).toEqual({ type: "buttons", buttons: ["a"], timestamps: [], seq: 1 });

		// A later command is a delta on the same track.
		game.heldButtons.delete("a");
		game.sendButtons();
		expect((await reader.next())?.value).toEqual({ type: "buttons", buttons: [], timestamps: [], seq: 2 });

		// A minute without input withdraws the broadcast, so the emulator counts this viewer gone.
		await clock.advance(Duration.secs(60));
		expect(game.viewerId.peek()).toBeUndefined();
		expect(await reader.next()).toBeUndefined();

		// The next input publishes under a new id, and leaving the tile withdraws it.
		game.sendCommand({ type: "reset" });
		await settle();
		const second = game.viewerId.peek();
		expect(second).toBeDefined();
		expect(second).not.toBe(first);
		const again = await commands(origin, second as string);
		expect((await again.next())?.value).toEqual({ type: "reset", timestamps: [], seq: 3 });

		// A repeated reset is its own command, not an unchanged value the producer skips.
		game.sendCommand({ type: "reset" });
		expect((await again.next())?.value).toEqual({ type: "reset", timestamps: [], seq: 4 });

		game.hovered.set(false);
		await settle();
		expect(game.viewerId.peek()).toBeUndefined();
		expect(await again.next()).toBeUndefined();
	} finally {
		game.close();
		origin.close();
		connection.close();
		installed.close();
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
	const game = new Game({
		sessionId: "test",
		connection,
		origin,
		expanded: new Moq.Signals.Signal<string | undefined>(undefined),
		gamePrefix: "game",
		viewerPrefix: "viewer",
	});

	try {
		game.hovered.set(true);
		game.sendCommand({ type: "reset" });
		await settle();

		status.set("connected");
		await settle(50);
		expect(game.viewerId.peek()).toBeUndefined();

		game.heldButtons.add("a");
		game.sendButtons();
		await settle();
		const reader = await commands(origin, game.viewerId.peek() as string);
		expect((await reader.next())?.value).toEqual({ type: "buttons", buttons: ["a"], timestamps: [], seq: 1 });
	} finally {
		game.close();
		origin.close();
		connection.close();
		if (originalDocument) Object.defineProperty(globalThis, "document", originalDocument);
		else Reflect.deleteProperty(globalThis, "document");
	}
});
