import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render } from "solid-js/web";
import { createSignal } from "solid-js";
import { StatsItem } from "../StatsItem";
import * as registry from "../../handlers/registry";
import type { HandlerContext, IStatsHandler } from "../../types";

vi.mock("../../handlers/registry", () => ({
	getHandlerClass: vi.fn(),
}));

describe("StatsItem", () => {
	let container: HTMLDivElement;

	beforeEach(() => {
		container = document.createElement("div");
		document.body.appendChild(container);
	});

	afterEach(() => {
		document.body.removeChild(container);
		vi.clearAllMocks();
	});

	it("renders with correct base structure", () => {
		vi.mocked(registry.getHandlerClass).mockReturnValue(undefined);

		render(
			() => (
				<StatsItem
					icon="network"
					svg="<svg><circle r='5'></circle></svg>"
				/>
			),
			container
		);

		const item = container.querySelector(".stats__item");
		expect(item).toBeTruthy();
		expect(item?.classList.contains("stats__item--network")).toBe(true);
	});

	it("renders icon wrapper with SVG content", () => {
		vi.mocked(registry.getHandlerClass).mockReturnValue(undefined);

		const testSvg = '<svg><circle r="5"></circle></svg>';
		render(
			() => (
				<StatsItem
					icon="video"
					svg={testSvg}
				/>
			),
			container
		);

		const iconWrapper = container.querySelector(".stats__icon-wrapper");
		expect(iconWrapper).toBeTruthy();

		const icon = container.querySelector(".stats__icon");
		expect(icon?.innerHTML).toBe(testSvg);
	});

	it("renders item detail with icon text", () => {
		vi.mocked(registry.getHandlerClass).mockReturnValue(undefined);

		render(
			() => (
				<StatsItem
					icon="audio"
					svg="<svg></svg>"
				/>
			),
			container
		);

		const iconText = container.querySelector(".stats__item-text");
		expect(iconText?.textContent).toBe("audio");
	});

	it("displays N/A when no handler is available", () => {
		vi.mocked(registry.getHandlerClass).mockReturnValue(undefined);

		render(
			() => (
				<StatsItem
					icon="buffer"
					svg="<svg></svg>"
				/>
			),
			container
		);

		const dataDisplay = container.querySelector(".stats__item-data");
		expect(dataDisplay?.textContent).toBe("N/A");
	});

	it("initializes handler with audio and video props", () => {
		const mockHandler: IStatsHandler = {
			setup: vi.fn(),
			cleanup: vi.fn(),
		};

		const MockHandlerClass = vi.fn(() => mockHandler) as ReturnType<
			typeof registry.getHandlerClass
		>;
		vi.mocked(registry.getHandlerClass).mockReturnValue(MockHandlerClass);

		const mockAudio = { active: { peek: () => "audio-data" } };
		const mockVideo = { display: { peek: () => ({ width: 1920, height: 1080 }) } };

		render(
			() => (
				<StatsItem
					icon="network"
					svg="<svg></svg>"
					audio={mockAudio}
					video={mockVideo}
				/>
			),
			container
		);

		expect(MockHandlerClass).toHaveBeenCalledWith({
			audio: mockAudio,
			video: mockVideo,
		});
	});

	it("calls handler setup with setDisplayData callback", () => {
		const mockHandler: IStatsHandler = {
			setup: vi.fn(),
			cleanup: vi.fn(),
		};

		const MockHandlerClass = vi.fn(() => mockHandler) as ReturnType<
			typeof registry.getHandlerClass
		>;
		vi.mocked(registry.getHandlerClass).mockReturnValue(MockHandlerClass);

		render(
			() => (
				<StatsItem
					icon="video"
					svg="<svg></svg>"
				/>
			),
			container
		);

		expect(mockHandler.setup).toHaveBeenCalled();

		const setupCall = vi.mocked(mockHandler.setup).mock.calls[0][0] as HandlerContext;
		expect(setupCall.setDisplayData).toBeDefined();
		expect(typeof setupCall.setDisplayData).toBe("function");
	});

	it("updates display data when handler calls setDisplayData", () => {
		let capturedSetDisplayData: ((data: string) => void) | undefined;

		const mockHandler: IStatsHandler = {
			setup: vi.fn((context: HandlerContext) => {
				capturedSetDisplayData = context.setDisplayData;
			}),
			cleanup: vi.fn(),
		};

		const MockHandlerClass = vi.fn(() => mockHandler) as ReturnType<
			typeof registry.getHandlerClass
		>;
		vi.mocked(registry.getHandlerClass).mockReturnValue(MockHandlerClass);

		render(
			() => (
				<StatsItem
					icon="audio"
					svg="<svg></svg>"
				/>
			),
			container
		);

		expect(capturedSetDisplayData).toBeDefined();

		capturedSetDisplayData!("42 kbps");

		const dataDisplay = container.querySelector(".stats__item-data");
		expect(dataDisplay?.textContent).toBe("42 kbps");
	});

	it("renders correct class for each icon type", () => {
		vi.mocked(registry.getHandlerClass).mockReturnValue(undefined);

		const icons = ["network", "video", "audio", "buffer"] as const;

		icons.forEach((icon) => {
			const testContainer = document.createElement("div");
			document.body.appendChild(testContainer);

			render(
				() => (
					<StatsItem
						icon={icon}
						svg="<svg></svg>"
					/>
				),
				testContainer
			);

			const item = testContainer.querySelector(".stats__item");
			expect(item?.classList.contains(`stats__item--${icon}`)).toBe(true);

			document.body.removeChild(testContainer);
		});
	});

	it("cleans up previous handler before initializing new one", () => {
		const mockHandler1: IStatsHandler = {
			setup: vi.fn(),
			cleanup: vi.fn(),
		};

		const mockHandler2: IStatsHandler = {
			setup: vi.fn(),
			cleanup: vi.fn(),
		};

		const [icon, setIcon] = createSignal<"network" | "video">("network");

		const MockHandlerClass1 = vi.fn(() => mockHandler1) as ReturnType<
			typeof registry.getHandlerClass
		>;
		const MockHandlerClass2 = vi.fn(() => mockHandler2) as ReturnType<
			typeof registry.getHandlerClass
		>;

		let callCount = 0;
		vi.mocked(registry.getHandlerClass).mockImplementation(() => {
			if (callCount === 0) {
				callCount++;
				return MockHandlerClass1;
			}
			return MockHandlerClass2;
		});

		render(
			() => (
				<StatsItem
					icon={icon()}
					svg="<svg></svg>"
				/>
			),
			container
		);

		expect(mockHandler1.setup).toHaveBeenCalled();

		callCount = 0;
		vi.mocked(registry.getHandlerClass).mockReturnValue(MockHandlerClass2);

		setIcon("video");

		expect(mockHandler1.cleanup).toHaveBeenCalled();
		expect(mockHandler2.setup).toHaveBeenCalled();
	});

	it("maintains correct DOM hierarchy", () => {
		vi.mocked(registry.getHandlerClass).mockReturnValue(undefined);

		render(
			() => (
				<StatsItem
					icon="network"
					svg="<svg></svg>"
				/>
			),
			container
		);

		const item = container.querySelector(".stats__item");
		expect(item?.children.length).toBe(2);

		const iconWrapper = item?.querySelector(".stats__icon-wrapper");
		expect(iconWrapper).toBeTruthy();
		expect(iconWrapper?.children.length).toBe(1);

		const detail = item?.querySelector(".stats__item-detail");
		expect(detail).toBeTruthy();
		expect(detail?.children.length).toBe(2);

		expect(detail?.querySelector(".stats__item-text")).toBeTruthy();
		expect(detail?.querySelector(".stats__item-data")).toBeTruthy();
	});

	it("calls getHandlerClass with correct icon", () => {
		vi.mocked(registry.getHandlerClass).mockReturnValue(undefined);

		render(
			() => (
				<StatsItem
					icon="buffer"
					svg="<svg></svg>"
				/>
			),
			container
		);

		expect(registry.getHandlerClass).toHaveBeenCalledWith("buffer");
	});

	it("renders with dynamic SVG updates", () => {
		vi.mocked(registry.getHandlerClass).mockReturnValue(undefined);

		const [svg, setSvg] = createSignal("<svg><circle r='5'></circle></svg>");

		render(
			() => (
				<StatsItem
					icon="network"
					svg={svg()}
				/>
			),
			container
		);

		let icon = container.querySelector(".stats__icon");
		expect(icon?.innerHTML).toContain("circle");

		setSvg("<svg><rect width='10' height='10'></rect></svg>");

		icon = container.querySelector(".stats__icon");
		expect(icon?.innerHTML).toContain("rect");
		expect(icon?.innerHTML).not.toContain("circle");
	});
});
