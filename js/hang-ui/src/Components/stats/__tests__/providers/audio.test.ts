import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AudioProvider } from "../../providers/audio";
import type { AudioConfig, AudioSource, AudioStats, ProviderContext, ProviderProps } from "../../types";
import { createMockProviderProps } from "../utils";

declare global {
	var __advanceTime: (ms: number) => void;
}

describe("AudioProvider", () => {
	let provider: AudioProvider;
	let context: ProviderContext;
	let setDisplayData: ReturnType<typeof vi.fn>;
	let intervalCallback: ((interval: number) => void) | null = null;
	let originalWindow: typeof window;
	let originalPerformance: typeof performance;
	let mockClearInterval: ReturnType<typeof vi.fn>;

	beforeEach(() => {
		originalWindow = global.window;
		originalPerformance = global.performance as unknown as Performance;
		setDisplayData = vi.fn();
		context = { setDisplayData };
		intervalCallback = null;

		// Mock window functions
		const mockSetInterval = vi.fn((callback: () => void) => {
			intervalCallback = callback as unknown as (interval: number) => void;
			return 1;
		});
		mockClearInterval = vi.fn();

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
		global.window = originalWindow;
		global.performance = originalPerformance as unknown as Performance;
	});

	it("should display N/A when audio source is not available", () => {
		const props: ProviderProps = {};
		provider = new AudioProvider(props);
		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("N/A");
	});

	it("should display audio config with stats placeholder on first call", () => {
		const audioConfig: AudioConfig = {
			sampleRate: 48000,
			numberOfChannels: 2,
			bitrate: 128000,
			codec: "opus",
		};

		const stats: AudioStats = { bytesReceived: 0 };

		const audio: AudioSource = {
			source: {
				active: {
					peek: () => "audio",
					subscribe: vi.fn(() => vi.fn()),
				},
				config: {
					peek: () => audioConfig,
					subscribe: vi.fn(() => vi.fn()),
				},
				stats: {
					peek: () => stats,
					subscribe: vi.fn(() => vi.fn()),
				},
			},
		};

		const props: ProviderProps = { audio };
		provider = new AudioProvider(props);
		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("48.0kHz\n2ch\nN/A\nopus");
	});

	it("should calculate bitrate from bytesReceived delta", () => {
		const audioConfig: AudioConfig = {
			sampleRate: 48000,
			numberOfChannels: 2,
			bitrate: 128000,
			codec: "opus",
		};

		const peekFn = vi.fn(() => ({ bytesReceived: 0 }) as AudioStats);

		const audio: AudioSource = {
			source: {
				active: {
					peek: () => "audio",
					subscribe: vi.fn(() => vi.fn()),
				},
				config: {
					peek: () => audioConfig,
					subscribe: vi.fn(() => vi.fn()),
				},
				stats: {
					peek: peekFn,
					subscribe: vi.fn(() => vi.fn()),
				},
			},
		};

		const props: ProviderProps = { audio };
		provider = new AudioProvider(props);

		// First call - start with bytes already flowing
		peekFn.mockReturnValue({ bytesReceived: 5000 } as AudioStats);
		provider.setup(context);
		expect(setDisplayData).toHaveBeenCalledWith("48.0kHz\n2ch\nN/A\nopus");

		// Simulate bytesReceived increasing: 6250 bytes delta = 200 kbps at 250ms interval
		// (6250 bytes * 8 bits/byte * 4) / 1000 = 200 kbps
		peekFn.mockReturnValue({ bytesReceived: 11250 } as AudioStats);
		global.__advanceTime(250);
		intervalCallback?.(250);
		expect(setDisplayData).toHaveBeenNthCalledWith(2, "48.0kHz\n2ch\n200kbps\nopus");

		// Increase bytes more: delta 3125 = 100 kbps
		peekFn.mockReturnValue({ bytesReceived: 14375 } as AudioStats);
		global.__advanceTime(250);
		intervalCallback?.(250);
		expect(setDisplayData).toHaveBeenNthCalledWith(3, "48.0kHz\n2ch\n100kbps\nopus");
	});

	it("should display N/A when active or config is missing", () => {
		const audio: AudioSource = {
			source: {
				active: {
					peek: () => undefined,
					subscribe: vi.fn(() => vi.fn()),
				},
				config: {
					peek: () => undefined,
					subscribe: vi.fn(() => vi.fn()),
				},
				stats: {
					peek: () => ({ bytesReceived: 0 }),
					subscribe: vi.fn(() => vi.fn()),
				},
			},
		};

		const props: ProviderProps = { audio };
		provider = new AudioProvider(props);
		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("N/A");
	});

	it("should handle mono audio", () => {
		const audioConfig: AudioConfig = {
			sampleRate: 44100,
			numberOfChannels: 1,
			bitrate: 128000,
			codec: "opus",
		};

		const audio: AudioSource = {
			source: {
				active: {
					peek: () => "audio",
					subscribe: vi.fn(() => vi.fn()),
				},
				config: {
					peek: () => audioConfig,
					subscribe: vi.fn(() => vi.fn()),
				},
				stats: {
					peek: () => ({ bytesReceived: 0 }),
					subscribe: vi.fn(() => vi.fn()),
				},
			},
		};

		const props: ProviderProps = { audio };
		provider = new AudioProvider(props);
		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("44.1kHz\n1ch\nN/A\nopus");
	});

	it("should format Mbps for high bitrates", () => {
		const audioConfig: AudioConfig = {
			sampleRate: 48000,
			numberOfChannels: 2,
			bitrate: 128000,
			codec: "opus",
		};

		const peekFn = vi.fn(() => ({ bytesReceived: 0 }) as AudioStats);

		const audio: AudioSource = {
			source: {
				active: {
					peek: () => "audio",
					subscribe: vi.fn(() => vi.fn()),
				},
				config: {
					peek: () => audioConfig,
					subscribe: vi.fn(() => vi.fn()),
				},
				stats: {
					peek: peekFn,
					subscribe: vi.fn(() => vi.fn()),
				},
			},
		};

		const props: ProviderProps = { audio };
		provider = new AudioProvider(props);

		// First call - display audio config with stats placeholder on first call
		peekFn.mockReturnValue({ bytesReceived: 48000 } as AudioStats);
		provider.setup(context);
		// Simulate 5 Mbps: 156250 bytes delta = 5000000 bits/s = 5 Mbps
		peekFn.mockReturnValue({ bytesReceived: 204250 } as AudioStats);
		global.__advanceTime(250);
		intervalCallback?.(250);

		expect(setDisplayData).toHaveBeenNthCalledWith(2, "48.0kHz\n2ch\n5.0Mbps\nopus");
	});

	it("should cleanup interval on dispose", () => {
		const props: ProviderProps = createMockProviderProps({ video: false });
		provider = new AudioProvider(props);
		provider.setup(context);

		provider.cleanup();

		expect(mockClearInterval).toHaveBeenCalledTimes(1);
		expect(mockClearInterval).toHaveBeenCalledWith(1);
	});
});
