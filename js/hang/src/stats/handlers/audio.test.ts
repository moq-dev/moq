import { describe, expect, it, vi, beforeEach } from "vitest";
import { AudioHandler } from "./audio";
import type { HandlerContext, HandlerProps, AudioSource, AudioConfig } from "../types";

describe("AudioHandler", () => {
	let handler: AudioHandler;
	let context: HandlerContext;
	let setDisplayData: ReturnType<typeof vi.fn>;

	beforeEach(() => {
		setDisplayData = vi.fn();
		context = { setDisplayData };
	});

	it("should display N/A when audio source is not available", () => {
		const props: HandlerProps = {};
		handler = new AudioHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("N/A");
	});

	it("should display audio config with bitrate", () => {
		const audioConfig: AudioConfig = {
			sampleRate: 48000,
			numberOfChannels: 2,
			bitrate: 128000,
			codec: "opus",
		};

		const audio: AudioSource = {
			active: {
				peek: () => "audio",
				subscribe: vi.fn(() => vi.fn()),
			},
			config: {
				peek: () => audioConfig,
				subscribe: vi.fn(() => vi.fn()),
			},
			bitrate: {
				peek: () => undefined,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { audio };
		handler = new AudioHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("48.0kHz\n2ch\n128kbps\nopus");
	});

	it("should use real-time bitrate when available", () => {
		const audioConfig: AudioConfig = {
			sampleRate: 48000,
			numberOfChannels: 2,
			bitrate: 128000,
			codec: "opus",
		};

		const audio: AudioSource = {
			active: {
				peek: () => "audio",
				subscribe: vi.fn(() => vi.fn()),
			},
			config: {
				peek: () => audioConfig,
				subscribe: vi.fn(() => vi.fn()),
			},
			bitrate: {
				peek: () => 127500,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { audio };
		handler = new AudioHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("48.0kHz\n2ch\n128kbps\nopus");
	});

	it("should calculate bitrate from channels if not provided", () => {
		const audioConfig: AudioConfig = {
			sampleRate: 44100,
			numberOfChannels: 2,
			codec: "opus",
		};

		const audio: AudioSource = {
			active: {
				peek: () => "audio",
				subscribe: vi.fn(() => vi.fn()),
			},
			config: {
				peek: () => audioConfig,
				subscribe: vi.fn(() => vi.fn()),
			},
			bitrate: {
				peek: () => undefined,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { audio };
		handler = new AudioHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("44.1kHz\n2ch\n64kbps\nopus");
	});

	it("should display N/A when active or config is missing", () => {
		const audio: AudioSource = {
			active: {
				peek: () => undefined,
				subscribe: vi.fn(() => vi.fn()),
			},
			config: {
				peek: () => undefined,
				subscribe: vi.fn(() => vi.fn()),
			},
			bitrate: {
				peek: () => undefined,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { audio };
		handler = new AudioHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("N/A");
	});

	it("should handle mono audio", () => {
		const audioConfig: AudioConfig = {
			sampleRate: 44100,
			numberOfChannels: 1,
			bitrate: 64000,
			codec: "opus",
		};

		const audio: AudioSource = {
			active: {
				peek: () => "audio",
				subscribe: vi.fn(() => vi.fn()),
			},
			config: {
				peek: () => audioConfig,
				subscribe: vi.fn(() => vi.fn()),
			},
			bitrate: {
				peek: () => undefined,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { audio };
		handler = new AudioHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("44.1kHz\n1ch\n64kbps\nopus");
	});
});
