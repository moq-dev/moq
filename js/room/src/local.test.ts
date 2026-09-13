import { expect, mock, test } from "bun:test";
import { Path } from "@moq/net";
import { Signal } from "@moq/signals";

const sources: FakeSource[] = [];
class FakeSource {
	out = { source: new Signal<unknown>(undefined) };
	constructor() {
		sources.push(this);
	}
	close() {}
}
class FakePipeline {
	out = { display: new Signal(undefined), frame: new Signal(undefined) };
	close() {}
}
class FakeBroadcast {
	net = new Signal(undefined);
	catalog = { mutate: (fn: (value: object) => void) => fn({}) };
	close() {}
}
// Exercise room lifecycle wiring independently of camera drivers and browser workers.
mock.module("../../publish/src/index.ts", () => ({
	Source: { Camera: FakeSource, Microphone: FakeSource, Screen: FakeSource },
	Video: { Capture: FakePipeline, Encoder: FakePipeline },
	Audio: { Encoder: FakePipeline },
	Broadcast: FakeBroadcast,
}));
const { Local } = await import("./local.ts");
async function flush() {
	for (let i = 0; i < 30; i++) await Promise.resolve();
}

test("screen capture stays enabled while pending and resets after a live share ends", async () => {
	const local = new Local({ connection: undefined, identity: Path.from("alice") });
	try {
		await flush();
		local.screenEnabled.set(true);
		await flush();
		expect(local.screenEnabled.peek()).toBe(true);
		const screen = sources.at(-1);
		screen?.out.source.set({ video: {} });
		await flush();
		expect(local.preview.peek().screen).toBe(true);
		screen?.out.source.set(undefined);
		await flush();
		expect(local.screenEnabled.peek()).toBe(false);
		expect(local.preview.peek().screen).toBe(false);
	} finally {
		local.close();
	}
});
