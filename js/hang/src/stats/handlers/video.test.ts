import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { VideoHandler } from "./video";
import type { HandlerContext, HandlerProps, VideoSource } from "../types";

describe("VideoHandler", () => {
	let handler: VideoHandler;
	let context: HandlerContext;
	let setDisplayData: ReturnType<typeof vi.fn>;

	beforeEach(() => {
		setDisplayData = vi.fn();
		context = { setDisplayData };

		const video: VideoSource = {
			display: {
				peek: () => ({ width: 1920, height: 1080 }),
				subscribe: vi.fn(() => vi.fn()),
			},
			fps: {
				peek: () => 30.5,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { video };
		handler = new VideoHandler(props);
	});

	afterEach(() => {
		vi.clearAllTimers();
		handler.cleanup();
	});

	it("should display N/A when video source is not available", () => {
		const props: HandlerProps = {};
		const handler = new VideoHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("N/A");
	});

	it("should setup interval for display updates", () => {
		vi.useFakeTimers();
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalled();
		vi.useRealTimers();
	});

	it("should display video resolution and FPS", () => {
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("1920x1080\n@30.5 fps");
	});

	it("should display N/A for FPS when not available", () => {
		const video: VideoSource = {
			display: {
				peek: () => ({ width: 1280, height: 720 }),
				subscribe: vi.fn(() => vi.fn()),
			},
			fps: {
				peek: () => undefined,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { video };
		const handler = new VideoHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("1280x720\nN/A");
	});

	it("should display only FPS when resolution is not available", () => {
		const video: VideoSource = {
			display: {
				peek: () => ({ width: 0, height: 0 }),
				subscribe: vi.fn(() => vi.fn()),
			},
			fps: {
				peek: () => 24.0,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { video };
		const handler = new VideoHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("@24.0 fps");
	});

	it("should round FPS to one decimal place", () => {
		const video: VideoSource = {
			display: {
				peek: () => ({ width: 1920, height: 1080 }),
				subscribe: vi.fn(() => vi.fn()),
			},
			fps: {
				peek: () => 23.9999,
				subscribe: vi.fn(() => vi.fn()),
			},
		};

		const props: HandlerProps = { video };
		const handler = new VideoHandler(props);
		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("1920x1080\n@24.0 fps");
	});

	it("should cleanup interval on dispose", () => {
		vi.useFakeTimers();
		handler.setup(context);
		const clearIntervalSpy = vi.spyOn(window, "clearInterval");

		handler.cleanup();

		expect(clearIntervalSpy).toHaveBeenCalled();
		vi.useRealTimers();
	});
});
