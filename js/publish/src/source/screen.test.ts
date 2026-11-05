import { expect, mock, test } from "bun:test";
import { Signal } from "@moq/signals";
import { Screen } from "./screen";

const settle = async () => {
	for (let i = 0; i < 20; i++) await Promise.resolve();
};

for (const cancel of ["close", "disable"] as const) {
	test(`a screen picked after ${cancel} releases every track`, async () => {
		const pending = Promise.withResolvers<MediaStream>();
		const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");
		const originalSelf = Object.getOwnPropertyDescriptor(globalThis, "self");
		Object.defineProperty(globalThis, "navigator", {
			configurable: true,
			value: { mediaDevices: { getDisplayMedia: () => pending.promise } },
		});
		Object.defineProperty(globalThis, "self", { configurable: true, value: {} });
		const enabled = new Signal(true);
		const screen = new Screen({ enabled });
		const tracks = [{ stop: mock(() => {}) }, { stop: mock(() => {}) }];
		try {
			await settle();
			if (cancel === "close") screen.close();
			else enabled.set(false);
			await settle();
			pending.resolve({ getTracks: () => tracks } as unknown as MediaStream);
			await settle();
			for (const track of tracks) expect(track.stop).toHaveBeenCalledTimes(1);
			expect(screen.out.source.peek()).toBeUndefined();
		} finally {
			screen.close();
			if (originalNavigator) Object.defineProperty(globalThis, "navigator", originalNavigator);
			else Reflect.deleteProperty(globalThis, "navigator");
			if (originalSelf) Object.defineProperty(globalThis, "self", originalSelf);
			else Reflect.deleteProperty(globalThis, "self");
		}
	});
}
