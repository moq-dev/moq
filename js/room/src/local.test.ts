import { expect, mock, test } from "bun:test";
import { Path } from "@moq/net";
import { Signal } from "@moq/signals";

const sources: FakeSource[] = [];
const encoders: Record<string, unknown>[] = [];
class FakeSource {
	out = { source: new Signal<unknown>(undefined), state: new Signal<unknown>({ kind: "off" }) };
	constructor() {
		sources.push(this);
	}
	close() {}
}
class FakePipeline {
	out = { display: new Signal(undefined), frame: new Signal(undefined) };
	constructor(name?: unknown, props?: Record<string, unknown>) {
		if (typeof name === "string" && props) encoders.push(props);
	}
	close() {}
}
const broadcasts: { enabled: Signal<boolean> }[] = [];
class FakeBroadcast {
	net = new Signal(undefined);
	catalog = { mutate: (fn: (value: object) => void) => fn({}) };
	constructor(props: { enabled: Signal<boolean> }) {
		broadcasts.push(props);
	}
	close() {}
}
// Exercise room lifecycle wiring independently of camera drivers and browser workers.
mock.module("../../publish/src/index.ts", () => ({
	Source: { Camera: FakeSource, Microphone: FakeSource, Screen: FakeSource },
	Video: { Capture: FakePipeline, Encoder: FakePipeline },
	Audio: { Capture: FakePipeline, Encoder: FakePipeline },
	Broadcast: FakeBroadcast,
}));
const { Local } = await import("./local.ts");
async function flush() {
	for (let i = 0; i < 30; i++) await Promise.resolve();
}

function setup() {
	// Each test counts only its own Local's sources and encoders.
	sources.length = 0;
	encoders.length = 0;
	broadcasts.length = 0;
	const bandwidth = new Signal(undefined);
	const local = new Local({
		connection: { origin: new Signal(undefined), allocator: bandwidth } as never,
		identity: Path.from("alice"),
	});
	const [camera, screenBroadcast] = broadcasts;
	if (!camera || !screenBroadcast) throw new Error("expected the camera and screen broadcasts");
	return { local, bandwidth, screen: sources.at(-1) as FakeSource, broadcasts: { camera, screen: screenBroadcast } };
}

// Replays LeavingStopsPublishing in `verify/Room.tla`: the screen broadcast is enabled by a live
// share alone, so a participant who never joined, or who left, still publishes their screen.
test("residual: a screen share publishes while not joined and after leaving", async () => {
	const { local, screen, broadcasts } = setup();
	try {
		local.screenEnabled.set(true);
		screen.out.state.set({ kind: "live", media: { video: {} } });
		await flush();
		expect(broadcasts.camera.enabled.peek()).toBe(false);
		expect(broadcasts.screen.enabled.peek()).toBe(true);

		local.enabled.set(true);
		await flush();
		local.enabled.set(false);
		await flush();
		expect(broadcasts.camera.enabled.peek()).toBe(false);
		expect(broadcasts.screen.enabled.peek()).toBe(true);
	} finally {
		local.close();
	}
});

test("screen capture stays enabled while prompting and resets after a live share ends", async () => {
	const { local, bandwidth, screen } = setup();
	try {
		await flush();
		expect(encoders).toHaveLength(6);
		expect(encoders.every((props) => props.bandwidth === bandwidth)).toBe(true);
		local.screenEnabled.set(true);
		screen.out.state.set({ kind: "prompting" });
		await flush();
		expect(local.screenEnabled.peek()).toBe(true);
		expect(local.preview.peek().screen).toBe(false);

		screen.out.state.set({ kind: "live", media: { video: {} } });
		await flush();
		expect(local.preview.peek().screen).toBe(true);

		screen.out.state.set({ kind: "ended", reason: "stopped" });
		await flush();
		expect(local.screenEnabled.peek()).toBe(false);
		expect(local.preview.peek().screen).toBe(false);
	} finally {
		local.close();
	}
});

// The share never went live, so nothing but its state says the picker is gone.
test("a dismissed screen picker turns sharing off", async () => {
	const { local, screen } = setup();
	try {
		local.screenEnabled.set(true);
		screen.out.state.set({ kind: "prompting" });
		await flush();
		screen.out.state.set({ kind: "ended", reason: "dismissed" });
		await flush();
		expect(local.screenEnabled.peek()).toBe(false);
		expect(local.preview.peek().screen).toBe(false);
	} finally {
		local.close();
	}
});
