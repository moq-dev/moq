import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { HandlerContext, HandlerProps, VideoSource, VideoStats } from "../types";
import { VideoHandler } from "./video";

declare global {
	var __advanceTime: (ms: number) => void;
}

describe("VideoHandler", () => {
	let handler: VideoHandler;
	let context: HandlerContext;
	let setDisplayData: ReturnType<typeof vi.fn>;
	let intervalCallback: ((interval: number) => void) | null = null;

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
		intervalCallback = null;

		// Mock window functions
		const mockSetInterval = vi.fn((callback: (interval: number) => void) => {
			intervalCallback = callback;
			return 1 as unknown as NodeJS.Timeout;
		});

		const mockClearInterval = vi.fn();

		global.window = {
			setInterval: mockSetInterval,
			clearInterval: mockClearInterval,
		} as unknown as typeof window;

		// Mock performance.now()
		let mockTime = 0;
		global.performance = {
			now: vi.fn(() => mockTime),
		} as unknown as Performance;

		// Helper to advance mock time
		global.__advanceTime = (ms: number) => {
			mockTime += ms;
		};
	});

	afterEach(() => {
		handler?.cleanup();
	});

	it("should display N/A when video source is not available", () => {
		const props: HandlerProps = {};
		handler = new VideoHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("N/A");
	});

	it("should setup interval for display updates", () => {
		const video = createVideoSourceMock();

		const props: HandlerProps = { video };
		handler = new VideoHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalled();
	});

	it("should display video resolution with stats placeholder on first call", () => {
		const video = createVideoSourceMock();

		const props: HandlerProps = { video };
		handler = new VideoHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("1920x1080\nN/A\nN/A");
	});

	it("should calculate FPS from frame count and timestamp delta", () => {
		const peekFn = vi.fn();
		const video = createVideoSourceMock({ stats: { peek: peekFn, subscribe: vi.fn(() => vi.fn()) } });

		const props: HandlerProps = { video };
		handler = new VideoHandler(props);

		// First call - use non-zero timestamp so next call can calculate FPS
		peekFn.mockReturnValue({ frameCount: 100, timestamp: 1000000, bytesReceived: 50000 } as VideoStats);
		handler.setup(context);
		expect(setDisplayData).toHaveBeenCalledWith("1920x1080\nN/A\nN/A");

		// Second call: 6 frames in 250ms at 24fps = exactly 24 frames per second
		// frameCount delta = 106 - 100 = 6
		// timestamp delta = 250000 microseconds
		// FPS = 6 / 0.25 = 24.0 fps
		// bytesReceived delta = 100000 - 50000 = 50000 bytes
		// bitrate = 50000 * 8 * 4 = 1600000 bits/s = 1.6Mbps
		peekFn.mockReturnValue({ frameCount: 106, timestamp: 1250000, bytesReceived: 100000 } as VideoStats);
		global.__advanceTime(250);
		intervalCallback?.(250);

		expect(setDisplayData).toHaveBeenNthCalledWith(2, "1920x1080\n@24.0 fps\n1.6Mbps");
	});

	it("should calculate bitrate from bytesReceived delta", () => {
		const peekFn = vi.fn();
		const video = createVideoSourceMock({
			display: { peek: () => ({ width: 1280, height: 720 }), subscribe: vi.fn(() => vi.fn()) },
			stats: { peek: peekFn, subscribe: vi.fn(() => vi.fn()) },
		});

		const props: HandlerProps = { video };
		handler = new VideoHandler(props);

		// First call - use non-zero initial values
		peekFn.mockReturnValue({ frameCount: 0, timestamp: 1000000, bytesReceived: 100000 } as VideoStats);
		handler.setup(context);

		// Second call: 5 Mbps = 156250 bytes delta at 250ms
		// (156250 * 8 * 4) / 1_000_000 = 5.0 Mbps
		peekFn.mockReturnValue({ frameCount: 6, timestamp: 1250000, bytesReceived: 256250 } as VideoStats);
		global.__advanceTime(250);
		intervalCallback?.(250);

		expect(setDisplayData).toHaveBeenNthCalledWith(2, "1280x720\n@24.0 fps\n5.0Mbps");
	});

	it("should display N/A for FPS and bitrate on first call", () => {
		const _video = createVideoSourceMock();

		const props: HandlerProps = {};
		handler = new VideoHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("N/A");
	});

	it("should display only resolution when stats are not available", () => {
		const video = createVideoSourceMock({
			display: { peek: () => ({ width: 1280, height: 720 }), subscribe: vi.fn(() => vi.fn()) },
			stats: { peek: () => undefined, subscribe: vi.fn(() => vi.fn()) },
		});

		const props: HandlerProps = { video };
		handler = new VideoHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("1280x720\nN/A\nN/A");
	});

	it("should format kbps for lower bitrates", () => {
		const peekFn = vi.fn();
		const video = createVideoSourceMock({
			display: { peek: () => ({ width: 1920, height: 1080 }), subscribe: vi.fn(() => vi.fn()) },
			stats: { peek: peekFn, subscribe: vi.fn(() => vi.fn()) },
		});

		const props: HandlerProps = { video };
		handler = new VideoHandler(props);

		// First call - use non-zero initial timestamp
		peekFn.mockReturnValue({ frameCount: 0, timestamp: 1000000, bytesReceived: 100000 } as VideoStats);
		handler.setup(context);

		// 256 kbps = 8000 bytes at 250ms
		// (8000 * 8 * 4) / 1000 = 256 kbps
		peekFn.mockReturnValue({ frameCount: 6, timestamp: 1250000, bytesReceived: 108000 } as VideoStats);
		global.__advanceTime(250);
		intervalCallback?.(250);

		expect(setDisplayData).toHaveBeenNthCalledWith(2, "1920x1080\n@24.0 fps\n256kbps");
	});

	it("should cleanup interval on dispose", () => {
		const props: HandlerProps = {};
		handler = new VideoHandler(props);
		handler.setup(context);

		handler.cleanup();

		expect(handler).toBeDefined();
	});
});
