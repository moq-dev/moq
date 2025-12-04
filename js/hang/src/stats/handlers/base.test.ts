import { describe, expect, it, vi, beforeEach } from "vitest";
import { BaseHandler } from "./base";
import type { HandlerContext, HandlerProps } from "../types";

class TestHandler extends BaseHandler {
	public setupCalled = false;
	public setupContext: HandlerContext | undefined;

	setup(context: HandlerContext): void {
		this.setupCalled = true;
		this.setupContext = context;
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

	it("should cleanup subscriptions", () => {
		handler.setup(context);
		handler.cleanup();
		expect(handler.setupCalled).toBe(true);
	});

	it("should implement IStatsHandler interface", () => {
		expect(handler.setup).toBeDefined();
		expect(handler.cleanup).toBeDefined();
		expect(typeof handler.setup).toBe("function");
		expect(typeof handler.cleanup).toBe("function");
	});
});
