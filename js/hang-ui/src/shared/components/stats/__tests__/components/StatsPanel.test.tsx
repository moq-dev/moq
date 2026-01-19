import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StatsPanel, statsDetailItems } from "../../components/StatsPanel";
import type { ProviderProps } from "../../types";
import { createMockProviderProps } from "../utils";

describe("StatsPanel", () => {
	let container: HTMLDivElement;
	let dispose: (() => void) | undefined;
	let mockAudioVideo: ProviderProps;

	beforeEach(() => {
		container = document.createElement("div");
		document.body.appendChild(container);
		mockAudioVideo = createMockProviderProps();

		// Mock fetch to prevent network requests for Icon SVG files
		global.fetch = vi.fn().mockResolvedValue({
			ok: true,
			text: () => Promise.resolve('<svg xmlns="http://www.w3.org/2000/svg"></svg>'),
		});
	});

	afterEach(() => {
		dispose?.();
		dispose = undefined;
		document.body.removeChild(container);
		vi.restoreAllMocks();
	});

	it("renders with correct base class", () => {
		dispose = render(() => <StatsPanel audio={mockAudioVideo.audio} video={mockAudioVideo.video} />, container);

		const panel = container.querySelector(".stats__panel");
		expect(panel).toBeTruthy();
	});

	it("renders all metric items", () => {
		dispose = render(() => <StatsPanel audio={mockAudioVideo.audio} video={mockAudioVideo.video} />, container);

		const items = container.querySelectorAll(".stats__item");
		expect(items.length).toBe(statsDetailItems.length);
	});

	it("renders items with correct icon types", () => {
		const expectedIcons = ["network", "video", "audio", "buffer"];
		dispose = render(() => <StatsPanel audio={mockAudioVideo.audio} video={mockAudioVideo.video} />, container);

		const items = container.querySelectorAll(".stats__item");
		items.forEach((item, index) => {
			expect(item.classList.contains(`stats__item--${expectedIcons[index]}`)).toBe(true);
		});
	});

	it("renders each item with icon wrapper", () => {
		dispose = render(() => <StatsPanel audio={mockAudioVideo.audio} video={mockAudioVideo.video} />, container);

		const wrappers = container.querySelectorAll(".stats__icon-wrapper");
		expect(wrappers.length).toBe(4);
	});

	it("renders each item with detail section", () => {
		dispose = render(() => <StatsPanel audio={mockAudioVideo.audio} video={mockAudioVideo.video} />, container);

		const details = container.querySelectorAll(".stats__item-detail");
		expect(details.length).toBe(4);
	});

	it("maintains correct DOM structure", () => {
		dispose = render(() => <StatsPanel audio={mockAudioVideo.audio} video={mockAudioVideo.video} />, container);

		const panel = container.querySelector(".stats__panel");
		const items = panel?.querySelectorAll(".stats__item");

		expect(panel?.children.length).toBe(4);
		items?.forEach((item) => {
			expect(item.querySelector(".stats__icon-wrapper")).toBeTruthy();
			expect(item.querySelector(".stats__item-detail")).toBeTruthy();
		});
	});
});
