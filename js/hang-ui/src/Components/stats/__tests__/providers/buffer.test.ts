import type { Getter } from "@moq/signals";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { BufferProvider } from "../../providers/buffer";
import type {
	BufferStatus,
	ProviderContext,
	ProviderProps,
	SyncStatus,
	VideoResolution,
	VideoSource,
	VideoStats,
} from "../../types";

describe("BufferProvider", () => {
	let provider: BufferProvider;
	let context: ProviderContext;
	let setDisplayData: ReturnType<typeof vi.fn>;

	/**
	 * Helper to create a complete VideoSource mock with all required properties
	 */
	const createVideoSourceMock = (overrides?: {
		display?: Partial<Getter<VideoResolution | undefined>>;
		syncStatus?: Partial<Getter<SyncStatus | undefined>>;
		bufferStatus?: Partial<Getter<BufferStatus | undefined>>;
		latency?: Partial<Getter<number | undefined>>;
		stats?: Partial<Getter<VideoStats | undefined>>;
	}): VideoSource => {
		const unsubscribe = vi.fn();
		const changedCallbacks: Map<string, () => void> = new Map();

		const createSignalMock = <T>(key: string, peek: () => T) => {
			const callbacks: Array<() => void> = [];
			return {
				peek,
				changed: vi.fn((fn: () => void) => {
					callbacks.push(fn);
					// Store the callback so we can trigger it later if needed
					changedCallbacks.set(key, () => {
						callbacks.forEach((cb) => {
							cb();
						});
					});
					return unsubscribe;
				}),
				subscribe: vi.fn(() => unsubscribe),
			};
		};

		const displaySignal = {
			...createSignalMock("display", () => ({ width: 1920, height: 1080 })),
			...overrides?.display,
		};
		const syncStatusSignal = {
			...createSignalMock("syncStatus", () => ({ state: "ready" as const })),
			...overrides?.syncStatus,
		};
		const bufferStatusSignal = {
			...createSignalMock("bufferStatus", () => ({ state: "filled" as const })),
			...overrides?.bufferStatus,
		};
		const latencySignal = {
			...createSignalMock("latency", () => 100),
			...overrides?.latency,
		};
		const statsSignal = {
			...createSignalMock("stats", () => ({ frameCount: 0, timestamp: 0, bytesReceived: 0 }) as VideoStats),
			...overrides?.stats,
		};

		return {
			source: {
				display: displaySignal,
				syncStatus: syncStatusSignal,
				bufferStatus: bufferStatusSignal,
				latency: latencySignal,
				stats: statsSignal,
			},
		};
	};

	beforeEach(() => {
		setDisplayData = vi.fn();
		context = { setDisplayData };
	});

	it("should display N/A when video source is not available", () => {
		const props: ProviderProps = {};
		provider = new BufferProvider(props);
		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("N/A");
	});

	it("should calculate buffer percentage from sync status", async () => {
		const unsubscribe = vi.fn();
		const video = createVideoSourceMock({
			syncStatus: {
				peek: () => ({
					state: "wait" as const,
					bufferDuration: 500,
				}),
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
			bufferStatus: {
				peek: () => ({ state: "empty" as const }),
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
			latency: {
				peek: () => 1000,
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
		});

		const props: ProviderProps = { video };
		provider = new BufferProvider(props);
		provider.setup(context);

		// Wait for the effect to run
		await Promise.resolve();

		expect(setDisplayData).toHaveBeenCalledWith("50%\n1000ms");
	});

	it("should display 100% when buffer is filled", async () => {
		const unsubscribe = vi.fn();
		const video = createVideoSourceMock({
			syncStatus: {
				peek: () => undefined,
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
			bufferStatus: {
				peek: () => ({ state: "filled" as const }),
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
			latency: {
				peek: () => 500,
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
		});

		const props: ProviderProps = { video };
		provider = new BufferProvider(props);
		provider.setup(context);

		// Wait for the effect to run
		await Promise.resolve();

		expect(setDisplayData).toHaveBeenCalledWith("100%\n500ms");
	});

	it("should display 0% when buffer is empty", async () => {
		const unsubscribe = vi.fn();
		const video = createVideoSourceMock({
			syncStatus: {
				peek: () => undefined,
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
			bufferStatus: {
				peek: () => ({ state: "empty" as const }),
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
			latency: {
				peek: () => 1000,
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
		});

		const props: ProviderProps = { video };
		provider = new BufferProvider(props);
		provider.setup(context);

		// Wait for the effect to run
		await Promise.resolve();

		expect(setDisplayData).toHaveBeenCalledWith("0%\n1000ms");
	});

	it("should cap buffer percentage at 100%", async () => {
		const unsubscribe = vi.fn();
		const video = createVideoSourceMock({
			syncStatus: {
				peek: () => ({
					state: "wait" as const,
					bufferDuration: 2000,
				}),
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
			bufferStatus: {
				peek: () => ({ state: "empty" as const }),
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
			latency: {
				peek: () => 1000,
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
		});

		const props: ProviderProps = { video };
		provider = new BufferProvider(props);
		provider.setup(context);

		// Wait for the effect to run
		await Promise.resolve();

		expect(setDisplayData).toHaveBeenCalledWith("100%\n1000ms");
	});

	it("should display N/A when latency is not available", async () => {
		const unsubscribe = vi.fn();
		const video = createVideoSourceMock({
			syncStatus: {
				peek: () => ({
					state: "wait" as const,
					bufferDuration: 500,
				}),
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
			bufferStatus: {
				peek: () => ({ state: "empty" as const }),
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
			latency: {
				peek: () => undefined,
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
		});

		const props: ProviderProps = { video };
		provider = new BufferProvider(props);
		provider.setup(context);

		// Wait for the effect to run
		await Promise.resolve();

		expect(setDisplayData).toHaveBeenCalledWith("0%\nN/A");
	});

	it("should calculate percentage correctly with decimal values", async () => {
		const unsubscribe = vi.fn();
		const video = createVideoSourceMock({
			syncStatus: {
				peek: () => ({
					state: "wait" as const,
					bufferDuration: 333,
				}),
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
			bufferStatus: {
				peek: () => ({ state: "empty" as const }),
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
			latency: {
				peek: () => 1000,
				changed: vi.fn(() => unsubscribe),
				subscribe: vi.fn(() => unsubscribe),
			},
		});

		const props: ProviderProps = { video };
		provider = new BufferProvider(props);
		provider.setup(context);

		// Wait for the effect to run
		await Promise.resolve();

		expect(setDisplayData).toHaveBeenCalledWith("33%\n1000ms");
	});
});
