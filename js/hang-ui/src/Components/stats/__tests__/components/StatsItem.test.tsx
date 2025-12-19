import { createSignal } from "solid-js";
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StatsItem } from "../../components/StatsItem";
import type { BaseProvider } from "../../providers/base";
import * as registry from "../../providers/registry";
import type { ProviderContext, ProviderProps } from "../../types";
import { createMockProviderProps } from "../utils";

vi.mock("../../providers/registry", () => ({
	getStatsInformationProvider: vi.fn(),
}));

describe("StatsItem", () => {
	let container: HTMLDivElement;
	let dispose: (() => void) | undefined;
	let mockAudioVideo: ProviderProps;

	beforeEach(() => {
		container = document.createElement("div");
		document.body.appendChild(container);
		mockAudioVideo = createMockProviderProps();
	});

	afterEach(() => {
		dispose?.();
		dispose = undefined;
		document.body.removeChild(container);
		vi.clearAllMocks();
	});

	it("renders with correct base structure", () => {
		vi.mocked(registry.getStatsInformationProvider).mockReturnValue(undefined);

		dispose = render(
			() => (
				<StatsItem
					name="Network"
					statProvider="network"
					svg={
						<svg aria-hidden="true">
							<circle r="5"></circle>
						</svg>
					}
					audio={mockAudioVideo.audio}
					video={mockAudioVideo.video}
				/>
			),
			container,
		);

		const item = container.querySelector(".stats__item");
		expect(item).toBeTruthy();
		expect(item?.classList.contains("stats__item--network")).toBe(true);
	});

	it("renders icon wrapper with SVG content", () => {
		vi.mocked(registry.getStatsInformationProvider).mockReturnValue(undefined);

		const testSvg = () => (
			<svg data-testid="test-icon" aria-hidden="true">
				<circle r="5"></circle>
			</svg>
		);

		dispose = render(
			() => (
				<StatsItem
					name="Video"
					statProvider="video"
					svg={testSvg()}
					audio={mockAudioVideo.audio}
					video={mockAudioVideo.video}
				/>
			),
			container,
		);

		const iconWrapper = container.querySelector(".stats__icon-wrapper");
		expect(iconWrapper).toBeTruthy();

		const icon = container.querySelector(".stats__icon svg[data-testid='test-icon']");
		expect(icon).toBeTruthy();
	});

	it("renders item detail with icon text", () => {
		vi.mocked(registry.getStatsInformationProvider).mockReturnValue(undefined);

		dispose = render(
			() => (
				<StatsItem
					name="Audio"
					statProvider="audio"
					svg={<svg aria-hidden="true"></svg>}
					audio={mockAudioVideo.audio}
					video={mockAudioVideo.video}
				/>
			),
			container,
		);

		const iconText = container.querySelector(".stats__item-title");
		expect(iconText?.textContent).toBe("audio");
	});

	it("displays N/A when no provider is available", () => {
		vi.mocked(registry.getStatsInformationProvider).mockReturnValue(undefined);

		dispose = render(
			() => (
				<StatsItem
					name="Buffer"
					statProvider="buffer"
					svg={<svg aria-hidden="true"></svg>}
					audio={mockAudioVideo.audio}
					video={mockAudioVideo.video}
				/>
			),
			container,
		);

		const dataDisplay = container.querySelector(".stats__item-data");
		expect(dataDisplay?.textContent).toBe("N/A");
	});

	it("initializes provider with audio and video props", () => {
		const mockProvider: Pick<BaseProvider, "setup" | "cleanup"> = {
			setup: vi.fn(),
			cleanup: vi.fn(),
		};

		const MockProviderClass = vi.fn(() => mockProvider) as unknown as ReturnType<
			typeof registry.getStatsInformationProvider
		>;
		vi.mocked(registry.getStatsInformationProvider).mockReturnValue(MockProviderClass);

		dispose = render(
			() => (
				<StatsItem
					name="Network"
					statProvider="network"
					svg={<svg aria-hidden="true"></svg>}
					audio={mockAudioVideo.audio}
					video={mockAudioVideo.video}
				/>
			),
			container,
		);

		expect(MockProviderClass).toHaveBeenCalledWith(mockAudioVideo);
	});

	it("calls provider setup with setDisplayData callback", () => {
		const mockProvider: Pick<BaseProvider, "setup" | "cleanup"> = {
			setup: vi.fn(),
			cleanup: vi.fn(),
		};

		const MockProviderClass = vi.fn(() => mockProvider) as unknown as ReturnType<
			typeof registry.getStatsInformationProvider
		>;
		vi.mocked(registry.getStatsInformationProvider).mockReturnValue(MockProviderClass);

		dispose = render(
			() => (
				<StatsItem
					name="Video"
					statProvider="video"
					svg={<svg aria-hidden="true"></svg>}
					audio={mockAudioVideo.audio}
					video={mockAudioVideo.video}
				/>
			),
			container,
		);

		expect(mockProvider.setup).toHaveBeenCalled();

		const setupCall = vi.mocked(mockProvider.setup).mock.calls[0][0] as ProviderContext;
		expect(setupCall.setDisplayData).toBeDefined();
		expect(typeof setupCall.setDisplayData).toBe("function");
	});

	it("updates display data when provider calls setDisplayData", () => {
		let capturedSetDisplayData: ((data: string) => void) | undefined;

		const mockProvider: Pick<BaseProvider, "setup" | "cleanup"> = {
			setup: vi.fn((context: ProviderContext) => {
				capturedSetDisplayData = context.setDisplayData;
			}),
			cleanup: vi.fn(),
		};

		const MockProviderClass = vi.fn(() => mockProvider) as unknown as ReturnType<
			typeof registry.getStatsInformationProvider
		>;
		vi.mocked(registry.getStatsInformationProvider).mockReturnValue(MockProviderClass);

		dispose = render(
			() => (
				<StatsItem
					name="Audio"
					statProvider="audio"
					svg={<svg aria-hidden="true"></svg>}
					audio={mockAudioVideo.audio}
					video={mockAudioVideo.video}
				/>
			),
			container,
		);

		expect(capturedSetDisplayData).toBeDefined();

		capturedSetDisplayData?.("42 kbps");

		const dataDisplay = container.querySelector(".stats__item-data");
		expect(dataDisplay?.textContent).toBe("42 kbps");
	});

	it("renders correct class for each icon type", () => {
		vi.mocked(registry.getStatsInformationProvider).mockReturnValue(undefined);

		const statsProvider = ["network", "video", "audio", "buffer"] as const;

		statsProvider.forEach((provider) => {
			const testContainer = document.createElement("div");
			document.body.appendChild(testContainer);

			const testDispose = render(
				() => (
					<StatsItem
						name={provider.charAt(0).toUpperCase() + provider.slice(1)}
						statProvider={provider}
						svg={<svg aria-hidden="true"></svg>}
						audio={mockAudioVideo.audio}
						video={mockAudioVideo.video}
					/>
				),
				testContainer,
			);

			const item = testContainer.querySelector(".stats__item");
			expect(item?.classList.contains(`stats__item--${provider}`)).toBe(true);

			testDispose();
			document.body.removeChild(testContainer);
		});
	});

	it("cleans up previous provider before initializing new one", async () => {
		const mockProvider1: Pick<BaseProvider, "setup" | "cleanup"> = {
			setup: vi.fn(),
			cleanup: vi.fn(),
		};

		const mockProvider2: Pick<BaseProvider, "setup" | "cleanup"> = {
			setup: vi.fn(),
			cleanup: vi.fn(),
		};

		const MockProviderClass1 = vi.fn(() => mockProvider1) as unknown as ReturnType<
			typeof registry.getStatsInformationProvider
		>;
		const MockProviderClass2 = vi.fn(() => mockProvider2) as unknown as ReturnType<
			typeof registry.getStatsInformationProvider
		>;

		let _callCount = 0;
		vi.mocked(registry.getStatsInformationProvider).mockImplementation(() => {
			if (_callCount === 0) {
				_callCount++;
				return MockProviderClass1;
			}
			return MockProviderClass2;
		});

		dispose = render(() => {
			const [statProvider, setStatProvider] = createSignal<"network" | "video">("network");

			// Switch provider after initial render
			setTimeout(() => {
				_callCount = 0;
				vi.mocked(registry.getStatsInformationProvider).mockReturnValue(MockProviderClass2);
				setStatProvider("video");
			}, 0);

			return (
				<StatsItem
					name={statProvider().charAt(0).toUpperCase() + statProvider().slice(1)}
					statProvider={statProvider()}
					svg={<svg aria-hidden="true"></svg>}
					audio={mockAudioVideo.audio}
					video={mockAudioVideo.video}
				/>
			);
		}, container);

		expect(mockProvider1.setup).toHaveBeenCalled();

		// Wait for setTimeout
		await new Promise((resolve) => setTimeout(resolve, 10));

		expect(mockProvider1.cleanup).toHaveBeenCalled();
		expect(mockProvider2.setup).toHaveBeenCalled();
	});

	it("maintains correct DOM hierarchy", () => {
		vi.mocked(registry.getStatsInformationProvider).mockReturnValue(undefined);

		dispose = render(
			() => (
				<StatsItem
					name="Network"
					statProvider="network"
					svg={<svg aria-hidden="true"></svg>}
					audio={mockAudioVideo.audio}
					video={mockAudioVideo.video}
				/>
			),
			container,
		);

		const item = container.querySelector(".stats__item");
		expect(item?.children.length).toBe(2);

		const iconWrapper = item?.querySelector(".stats__icon-wrapper");
		expect(iconWrapper).toBeTruthy();
		expect(iconWrapper?.children.length).toBe(1);

		const detail = item?.querySelector(".stats__item-detail");
		expect(detail).toBeTruthy();
		expect(detail?.children.length).toBe(2);

		expect(detail?.querySelector(".stats__item-title")).toBeTruthy();
		expect(detail?.querySelector(".stats__item-data")).toBeTruthy();
	});

	it("calls getStatsInformationProvider with correct statProvider", () => {
		vi.mocked(registry.getStatsInformationProvider).mockReturnValue(undefined);

		dispose = render(() => <StatsItem name="Buffer" statProvider="buffer" svg="<svg></svg>" />, container);

		expect(registry.getStatsInformationProvider).toHaveBeenCalledWith("buffer");
	});
});
