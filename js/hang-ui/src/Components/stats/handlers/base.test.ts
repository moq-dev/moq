import { beforeEach, describe, expect, it, vi } from "vitest";
import type { HandlerContext, HandlerProps } from "../types";
import { BaseHandler } from "./base";

class TestHandler extends BaseHandler {
	public setupCalled = false;
	public setupContext: HandlerContext | undefined;
	public cleanupCalled = false;

	setup(context: HandlerContext): void {
		this.setupCalled = true;
		this.setupContext = context;
	}

	cleanup(): void {
		this.cleanupCalled = true;
		super.cleanup();
	}
}

describe("BaseHandler", () => {
	let handler: TestHandler;
	let context: HandlerContext;

	beforeEach(() => {
		const props: HandlerProps = {};
		handler = new TestHandler(props);
		context = {
			setDisplayData: vi.fn(),
		};
	});

	it("should initialize with props", () => {
		const props: HandlerProps = { audio: undefined, video: undefined };
		const testHandler = new TestHandler(props);
		expect(testHandler).toBeDefined();
	});

	it("should call setup method", () => {
		handler.setup(context);
		expect(handler.setupCalled).toBe(true);
		expect(handler.setupContext).toEqual(context);
	});

	it("should cleanup", () => {
		handler.setup(context);
		expect(handler.cleanupCalled).toBe(false);
		handler.cleanup();
		expect(handler.cleanupCalled).toBe(true);
		expect(handler.setupCalled).toBe(true);
	});

	it("should implement IStatsHandler interface", () => {
		expect(handler.setup).toBeDefined();
		expect(handler.cleanup).toBeDefined();
		expect(typeof handler.setup).toBe("function");
		expect(typeof handler.cleanup).toBe("function");
	});
});
