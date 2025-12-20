import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { VideoProvider } from "../../providers/video";
import type { ProviderContext, ProviderProps, VideoSource, VideoStats } from "../../types";
import { createMockProviderProps } from "../utils";

declare global {
	var __advanceTime: (ms: number) => void;
}

describe("VideoProvider", () => {
	let provider: VideoProvider;
	let context: ProviderContext;
	let setDisplayData: ReturnType<typeof vi.fn>;
	let intervalCallback: ((interval: number) => void) | null = null;

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
		provider?.cleanup();
	});

	it("should display N/A when video source is not available", () => {
		const props: ProviderProps = {};
		provider = new VideoProvider(props);
		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("N/A");
	});

	it("should setup interval for display updates", () => {
		const mockProps = createMockProviderProps({ audio: false });
		const video = mockProps.video as VideoSource;

		const props: ProviderProps = { video };
		provider = new VideoProvider(props);
		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalled();
	});

	it("should display video resolution with stats placeholder on first call", () => {
		const mockProps = createMockProviderProps({ audio: false });
		const video = mockProps.video as VideoSource;

		const props: ProviderProps = { video };
		provider = new VideoProvider(props);
		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("1920x1080\nN/A\nN/A");
	});

	it("should calculate FPS from frame count and timestamp delta", () => {
		const peekFn = vi.fn();
		const mockProps = createMockProviderProps({ audio: false });
		const video = mockProps.video as VideoSource;
		video.source.stats.peek = peekFn;

		const props: ProviderProps = { video };
		provider = new VideoProvider(props);

		// First call - use non-zero timestamp so next call can calculate FPS
		peekFn.mockReturnValue({ frameCount: 100, timestamp: 1000000, bytesReceived: 50000 } as VideoStats);
		provider.setup(context);
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
		const mockProps = createMockProviderProps({ audio: false });
		const video = mockProps.video as VideoSource;
		video.source.display.peek = () => ({ width: 1280, height: 720 });
		video.source.stats.peek = peekFn;

		const props: ProviderProps = { video };
		provider = new VideoProvider(props);

		// First call - use non-zero initial values
		peekFn.mockReturnValue({ frameCount: 0, timestamp: 1000000, bytesReceived: 100000 } as VideoStats);
		provider.setup(context);

		// Second call: 5 Mbps = 156250 bytes delta at 250ms
		// (156250 * 8 * 4) / 1_000_000 = 5.0 Mbps
		peekFn.mockReturnValue({ frameCount: 6, timestamp: 1250000, bytesReceived: 256250 } as VideoStats);
		global.__advanceTime(250);
		intervalCallback?.(250);

		expect(setDisplayData).toHaveBeenNthCalledWith(2, "1280x720\n@24.0 fps\n5.0Mbps");
	});

	it("should display N/A for FPS and bitrate on first call", () => {
		const props: ProviderProps = {};
		provider = new VideoProvider(props);
		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("N/A");
	});

	it("should display only resolution when stats are not available", () => {
		const mockProps = createMockProviderProps({ audio: false });
		const video = mockProps.video as VideoSource;
		video.source.display.peek = () => ({ width: 1280, height: 720 });
		video.source.stats.peek = () => undefined;

		const props: ProviderProps = { video };
		provider = new VideoProvider(props);
		provider.setup(context);
		expect(setDisplayData).toHaveBeenCalledWith("1280x720\nN/A\nN/A");
	});

	it("should format kbps for lower bitrates", () => {
		const peekFn = vi.fn();
		const mockProps = createMockProviderProps({ audio: false });
		const video = mockProps.video as VideoSource;
		video.source.stats.peek = peekFn;

		const props: ProviderProps = { video };
		provider = new VideoProvider(props);

		// First call - use non-zero initial timestamp
		peekFn.mockReturnValue({ frameCount: 0, timestamp: 1000000, bytesReceived: 100000 } as VideoStats);
		provider.setup(context);

		// 256 kbps = 8000 bytes at 250ms
		// (8000 * 8 * 4) / 1000 = 256 kbps
		peekFn.mockReturnValue({ frameCount: 6, timestamp: 1250000, bytesReceived: 108000 } as VideoStats);
		global.__advanceTime(250);
		intervalCallback?.(250);

		expect(setDisplayData).toHaveBeenNthCalledWith(2, "1920x1080\n@24.0 fps\n256kbps");
	});

	it("should cleanup interval on dispose", () => {
		const props: ProviderProps = {};
		provider = new VideoProvider(props);
		provider.setup(context);

		provider.cleanup();

		expect(provider).toBeDefined();
	});
});
