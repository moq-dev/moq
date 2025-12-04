import { describe, expect, it } from "vitest";
import { handlerRegistry, getHandlerClass } from "./registry";
import { VideoHandler } from "./video";
import { AudioHandler } from "./audio";
import { BufferHandler } from "./buffer";
import { NetworkHandler } from "./network";
import type { Icons } from "../types";

describe("Handler Registry", () => {
	it("should have all required handlers registered", () => {
		const expectedHandlers: Icons[] = ["video", "audio", "buffer", "network"];

		for (const icon of expectedHandlers) {
			expect(handlerRegistry[icon]).toBeDefined();
		}
	});

	it("should map video icon to VideoHandler", () => {
		expect(handlerRegistry.video).toBe(VideoHandler);
	});

	it("should map audio icon to AudioHandler", () => {
		expect(handlerRegistry.audio).toBe(AudioHandler);
	});

	it("should map buffer icon to BufferHandler", () => {
		expect(handlerRegistry.buffer).toBe(BufferHandler);
	});

	it("should map network icon to NetworkHandler", () => {
		expect(handlerRegistry.network).toBe(NetworkHandler);
	});

	it("should return correct handler class with getHandlerClass", () => {
		expect(getHandlerClass("video")).toBe(VideoHandler);
		expect(getHandlerClass("audio")).toBe(AudioHandler);
		expect(getHandlerClass("buffer")).toBe(BufferHandler);
		expect(getHandlerClass("network")).toBe(NetworkHandler);
	});

	it("should return undefined for unknown icon", () => {
		expect(getHandlerClass("unknown" as Icons)).toBeUndefined();
	});

	it("should instantiate handlers correctly", () => {
		const handlers = ["video", "audio", "buffer", "network"] as const;

		for (const icon of handlers) {
			const HandlerClass = getHandlerClass(icon);
			expect(HandlerClass).toBeDefined();

			if (HandlerClass) {
				const instance = new HandlerClass({});
				expect(instance).toBeDefined();
				expect(instance.setup).toBeDefined();
				expect(instance.cleanup).toBeDefined();
			}
		}
	});
});
