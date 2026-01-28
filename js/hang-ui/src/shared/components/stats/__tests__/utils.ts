import { vi } from "vitest";
import type { ProviderProps } from "../types";

export interface MockProviderPropsOptions {
	audio?: boolean;
	video?: boolean;
}

/**
 * Creates mock ProviderProps with configurable audio and video sources.
 * Each signal includes a peek() method and a subscribe() method with vi.fn().
 *
 * @param options - Configuration options
 * @returns ProviderProps with mocked audio and/or video sources
 *
 * @example
 * ```ts
 * // Create mock with both audio and video
 * const props = createMockProviderProps();
 *
 * // Create mock with audio only
 * const audioOnly = createMockProviderProps({ video: false });
 *
 * // Create mock with video only
 * const videoOnly = createMockProviderProps({ audio: false });
 * ```
 */
export const createMockProviderProps = (options?: MockProviderPropsOptions): ProviderProps => {
	const { audio = true, video = true } = options ?? {};

	return {
		...(audio && {
			audio: {
				source: {
					active: {
						peek: () => "audio-active",
						subscribe: vi.fn(() => vi.fn()),
						changed: vi.fn(() => vi.fn()),
					},
					config: {
						peek: () => ({
							sampleRate: 48000,
							numberOfChannels: 2,
							bitrate: 128000,
							codec: "opus",
						}),
						subscribe: vi.fn(() => vi.fn()),
						changed: vi.fn(() => vi.fn()),
					},
					stats: {
						peek: () => ({
							bytesReceived: 1024,
						}),
						subscribe: vi.fn(() => vi.fn()),
						changed: vi.fn(() => vi.fn()),
					},
				},
			},
		}),
		...(video && {
			video: {
				source: {
					display: {
						peek: () => ({
							width: 1920,
							height: 1080,
						}),
						subscribe: vi.fn(() => vi.fn()),
						changed: vi.fn(() => vi.fn()),
					},
					bufferStatus: {
						peek: () => ({ state: "filled" as const }),
						subscribe: vi.fn(() => vi.fn()),
						changed: vi.fn(() => vi.fn()),
					},
					stats: {
						peek: () => ({
							frameCount: 60,
							timestamp: Date.now(),
							bytesReceived: 2048,
						}),
						subscribe: vi.fn(() => vi.fn()),
						changed: vi.fn(() => vi.fn()),
					},
				},
			},
		}),
	};
};
