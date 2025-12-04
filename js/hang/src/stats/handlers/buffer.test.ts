import { describe, expect, it, vi, beforeEach } from "vitest";
import { BufferHandler } from "./buffer";
import type { HandlerContext, HandlerProps, VideoSource } from "../types";

describe("BufferHandler", () => {
	let handler: BufferHandler;
	let context: HandlerContext;
	let setDisplayData: ReturnType<typeof vi.fn>;

	beforeEach(() => {
		setDisplayData = vi.fn();
		context = { setDisplayData };
	});

	it("should display N/A when video source is not available", () => {
		const props: HandlerProps = {};
		handler = new BufferHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("N/A");
	});

	it("should calculate buffer percentage from sync status", () => {
		const video: VideoSource = {
			syncStatus: {
				peek: () => ({
					state: "wait" as const,
					bufferDuration: 500,
				}),
				subscribe: vi.fn(() => vi.fn()),
			},
			bufferStatus: {
				peek: () => ({ state: "empty" as const }),
				subscribe: vi.fn(() => vi.fn()),
			},
			latency: {
				peek: () => 1000,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { video };
		handler = new BufferHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("50%\n1000ms");
	});

	it("should display 100% when buffer is filled", () => {
		const video: VideoSource = {
			syncStatus: {
				peek: () => undefined,
				subscribe: vi.fn(() => vi.fn()),
			},
			bufferStatus: {
				peek: () => ({ state: "filled" as const }),
				subscribe: vi.fn(() => vi.fn()),
			},
			latency: {
				peek: () => 500,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { video };
		handler = new BufferHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("100%\n500ms");
	});

	it("should display 0% when buffer is empty", () => {
		const video: VideoSource = {
			syncStatus: {
				peek: () => undefined,
				subscribe: vi.fn(() => vi.fn()),
			},
			bufferStatus: {
				peek: () => ({ state: "empty" as const }),
				subscribe: vi.fn(() => vi.fn()),
			},
			latency: {
				peek: () => 1000,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { video };
		handler = new BufferHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("0%\n1000ms");
	});

	it("should cap buffer percentage at 100%", () => {
		const video: VideoSource = {
			syncStatus: {
				peek: () => ({
					state: "wait" as const,
					bufferDuration: 2000,
				}),
				subscribe: vi.fn(() => vi.fn()),
			},
			bufferStatus: {
				peek: () => ({ state: "empty" as const }),
				subscribe: vi.fn(() => vi.fn()),
			},
			latency: {
				peek: () => 1000,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { video };
		handler = new BufferHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("100%\n1000ms");
	});

	it("should display N/A when latency is not available", () => {
		const video: VideoSource = {
			syncStatus: {
				peek: () => ({
					state: "wait" as const,
					bufferDuration: 500,
				}),
				subscribe: vi.fn(() => vi.fn()),
			},
			bufferStatus: {
				peek: () => ({ state: "empty" as const }),
				subscribe: vi.fn(() => vi.fn()),
			},
			latency: {
				peek: () => undefined,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { video };
		handler = new BufferHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("0%\nN/A");
	});

	it("should subscribe to buffer status changes", () => {
		const unsubscribe = vi.fn();
		const subscribeFn = vi.fn(() => unsubscribe);

		const video: VideoSource = {
			syncStatus: {
				peek: () => undefined,
				subscribe: subscribeFn,
			},
			bufferStatus: {
				peek: () => ({ state: "filled" as const }),
				subscribe: subscribeFn,
			},
			latency: {
				peek: () => 1000,
				subscribe: subscribeFn,
			},
		};

		const props: HandlerProps = { video };
		handler = new BufferHandler(props);
		handler.setup(context);

		expect(subscribeFn).toHaveBeenCalledTimes(3);
	});

	it("should calculate percentage correctly with decimal values", () => {
		const video: VideoSource = {
			syncStatus: {
				peek: () => ({
					state: "wait" as const,
					bufferDuration: 333,
				}),
				subscribe: vi.fn(() => vi.fn()),
			},
			bufferStatus: {
				peek: () => ({ state: "empty" as const }),
				subscribe: vi.fn(() => vi.fn()),
			},
			latency: {
				peek: () => 1000,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { video };
		handler = new BufferHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("33%\n1000ms");
	});

	it("should cleanup subscriptions", () => {
		const unsubscribe = vi.fn();

		const video: VideoSource = {
			syncStatus: {
				peek: () => undefined,
				subscribe: () => unsubscribe,
			},
			bufferStatus: {
				peek: () => ({ state: "filled" as const }),
				subscribe: () => unsubscribe,
			},
			latency: {
				peek: () => 1000,
				subscribe: () => unsubscribe,
			},
		};

		const props: HandlerProps = { video };
		handler = new BufferHandler(props);
		handler.setup(context);
		handler.cleanup();

		expect(unsubscribe).toHaveBeenCalledTimes(3);
	});
});
