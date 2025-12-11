import { beforeEach, describe, expect, it, vi } from "vitest";
import type { HandlerContext, HandlerProps, VideoSource, VideoStats } from "../types";
import { BufferHandler } from "./buffer";

describe("BufferHandler", () => {
	let handler: BufferHandler;
	let context: HandlerContext;
	let setDisplayData: ReturnType<typeof vi.fn>;

	/**
	 * Helper to create a complete VideoSource mock with all required properties
	 */
	const createVideoSourceMock = (overrides?: Partial<VideoSource["source"]>): VideoSource => ({
		source: {
			display: {
				peek: () => ({ width: 1920, height: 1080 }),
				subscribe: vi.fn(() => vi.fn()),
				...overrides?.display,
			},
			syncStatus: {
				peek: () => ({ state: "ready" }),
				subscribe: vi.fn(() => vi.fn()),
				...overrides?.syncStatus,
			},
			bufferStatus: {
				peek: () => ({ state: "filled" }),
				subscribe: vi.fn(() => vi.fn()),
				...overrides?.bufferStatus,
			},
			latency: {
				peek: () => 100,
				subscribe: vi.fn(() => vi.fn()),
				...overrides?.latency,
			},
			stats: {
				peek: () => ({ frameCount: 0, timestamp: 0, bytesReceived: 0 }) as VideoStats,
				subscribe: vi.fn(() => vi.fn()),
				...overrides?.stats,
			},
		},
	});

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
		const video = createVideoSourceMock({
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
		});

		const props: HandlerProps = { video };
		handler = new BufferHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("50%\n1000ms");
	});

	it("should display 100% when buffer is filled", () => {
		const video = createVideoSourceMock({
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
		});

		const props: HandlerProps = { video };
		handler = new BufferHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("100%\n500ms");
	});

	it("should display 0% when buffer is empty", () => {
		const video = createVideoSourceMock({
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
		});

		const props: HandlerProps = { video };
		handler = new BufferHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("0%\n1000ms");
	});

	it("should cap buffer percentage at 100%", () => {
		const video = createVideoSourceMock({
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
		});

		const props: HandlerProps = { video };
		handler = new BufferHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("100%\n1000ms");
	});

	it("should display N/A when latency is not available", () => {
		const video = createVideoSourceMock({
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
		});

		const props: HandlerProps = { video };
		handler = new BufferHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("0%\nN/A");
	});

	it("should calculate percentage correctly with decimal values", () => {
		const video = createVideoSourceMock({
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
		});

		const props: HandlerProps = { video };
		handler = new BufferHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("33%\n1000ms");
	});
});
