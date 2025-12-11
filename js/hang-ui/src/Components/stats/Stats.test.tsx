import { createContext, createSignal } from "solid-js";
import { render } from "solid-js/web";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { Stats } from "./Stats";
import type { HandlerProps } from "./types";

/**
 * Mock HandlerProps with audio and video data
 */
const createMockHandlerProps = (): HandlerProps => ({
	audio: {
		source: {
			active: {
				peek: () => "audio-active",
				subscribe: vi.fn(() => vi.fn()),
			},
			config: {
				peek: () => ({
					sampleRate: 48000,
					numberOfChannels: 2,
					bitrate: 128000,
					codec: "aac",
				}),
				subscribe: vi.fn(() => vi.fn()),
			},
			stats: {
				peek: () => ({
					bytesReceived: 0,
				}),
				subscribe: vi.fn(() => vi.fn()),
			},
		},
	},
	video: {
		source: {
			display: {
				peek: () => ({
					width: 1920,
					height: 1080,
				}),
				subscribe: vi.fn(() => vi.fn()),
			},
			syncStatus: {
				peek: () => ({ state: "ready" as const }),
				subscribe: vi.fn(() => vi.fn()),
			},
			bufferStatus: {
				peek: () => ({ state: "filled" as const }),
				subscribe: vi.fn(() => vi.fn()),
			},
			latency: {
				peek: () => 100,
				subscribe: vi.fn(() => vi.fn()),
			},
			stats: {
				peek: () => ({
					frameCount: 0,
					timestamp: 0,
					bytesReceived: 0,
				}),
				subscribe: vi.fn(() => vi.fn()),
			},
		},
	},
});

describe("Stats Component", () => {
	let container: HTMLDivElement;

	beforeEach(() => {
		container = document.createElement("div");
		document.body.appendChild(container);
	});

	it("renders stats container", () => {
		const mockProps = createMockHandlerProps();
		const TestContext = createContext<HandlerProps>(mockProps);

		render(
			() => (
				<TestContext.Provider value={mockProps}>
					<Stats<HandlerProps> context={TestContext} getElement={() => mockProps} />
				</TestContext.Provider>
			),
			container,
		);

		const stats = container.querySelector(".stats");
		expect(stats).toBeTruthy();
	});

	it("waits for audio and video before rendering", async () => {
		const mockDefault = createMockHandlerProps();
		const TestContext = createContext<HandlerProps>(mockDefault);
		const [mediaElement, setMediaElement] = createSignal<HandlerProps | undefined>(undefined);

		render(
			() => (
				<TestContext.Provider value={mediaElement() ?? createMockHandlerProps()}>
					<Stats<HandlerProps> context={TestContext} getElement={() => mediaElement()} />
				</TestContext.Provider>
			),
			container,
		);

		// Initially no StatsWrapper should be rendered
		let wrapper = container.querySelector(".stats__wrapper");
		expect(wrapper).toBeFalsy();

		// Set media element and wait for effect
		await new Promise((resolve) => setTimeout(resolve, 100));
		setMediaElement(createMockHandlerProps());

		await new Promise((resolve) => setTimeout(resolve, 100));
		// Now wrapper should be rendered
		wrapper = container.querySelector(".stats__wrapper");
		expect(wrapper).toBeTruthy();
	});

	it("only renders when both audio and video are available", async () => {
		const mockDefault = createMockHandlerProps();
		const TestContext = createContext<HandlerProps>(mockDefault);
		const mockWithoutVideo: Partial<HandlerProps> = {
			audio: {
				source: {
					active: {
						peek: () => "audio-active",
						subscribe: vi.fn(() => vi.fn()),
					},
					config: {
						peek: () => ({
							sampleRate: 48000,
							numberOfChannels: 2,
							bitrate: 128000,
							codec: "aac",
						}),
						subscribe: vi.fn(() => vi.fn()),
					},
					stats: {
						peek: () => ({
							bytesReceived: 0,
						}),
						subscribe: vi.fn(() => vi.fn()),
					},
				},
			},
		};

		const [mediaElement, setMediaElement] = createSignal<Partial<HandlerProps> | undefined>(mockWithoutVideo);

		render(
			() => (
				<TestContext.Provider value={mediaElement() as HandlerProps}>
					<Stats<HandlerProps>
						context={TestContext}
						getElement={() => mediaElement() as HandlerProps | undefined}
					/>
				</TestContext.Provider>
			),
			container,
		);

		let wrapper = container.querySelector(".stats__wrapper");

		await new Promise((resolve) => setTimeout(resolve, 100));
		// Now set full props with both audio and video
		setMediaElement(createMockHandlerProps());

		await new Promise((resolve) => setTimeout(resolve, 100));
		wrapper = container.querySelector(".stats__wrapper");
		expect(wrapper).toBeTruthy();
	});

	it("works with different context types", () => {
		interface CustomContext {
			hangWatch: () => HandlerProps | undefined;
		}

		const mockProps = createMockHandlerProps();
		const contextValue: CustomContext = {
			hangWatch: () => mockProps,
		};
		const CustomTestContext = createContext<CustomContext>(contextValue);

		render(
			() => (
				<CustomTestContext.Provider value={contextValue}>
					<Stats<CustomContext> context={CustomTestContext} getElement={(ctx) => ctx?.hangWatch()} />
				</CustomTestContext.Provider>
			),
			container,
		);

		const stats = container.querySelector(".stats");
		expect(stats).toBeTruthy();

		const wrapper = container.querySelector(".stats__wrapper");
		expect(wrapper).toBeTruthy();
	});

	it("applies styles inline", () => {
		const mockProps = createMockHandlerProps();
		const TestContext = createContext<HandlerProps>(mockProps);

		render(
			() => (
				<TestContext.Provider value={mockProps}>
					<Stats<HandlerProps> context={TestContext} getElement={() => mockProps} />
				</TestContext.Provider>
			),
			container,
		);

		const style = container.querySelector(".stats style");
		expect(style).toBeTruthy();
	});

	it("provides context value to child components", () => {
		const mockProps = createMockHandlerProps();
		const TestContext = createContext<HandlerProps>(mockProps);

		render(
			() => (
				<TestContext.Provider value={mockProps}>
					<Stats<HandlerProps> context={TestContext} getElement={() => mockProps} />
				</TestContext.Provider>
			),
			container,
		);

		const wrapper = container.querySelector(".stats__wrapper");
		expect(wrapper).toBeTruthy();
	});

	it("handles undefined context gracefully", () => {
		const mockDefault = createMockHandlerProps();
		const TestContext = createContext<HandlerProps>(mockDefault);

		render(
			() => (
				<TestContext.Provider value={createMockHandlerProps()}>
					<Stats<HandlerProps> context={TestContext} getElement={() => undefined} />
				</TestContext.Provider>
			),
			container,
		);

		const stats = container.querySelector(".stats");
		expect(stats).toBeTruthy();

		// Should not render wrapper when element is undefined
		const wrapper = container.querySelector(".stats__wrapper");
		expect(wrapper).toBeFalsy();
	});

	it("updates when context changes", async () => {
		const mockDefault = createMockHandlerProps();
		const TestContext = createContext<HandlerProps>(mockDefault);
		const [element, setElement] = createSignal<HandlerProps | undefined>(undefined);

		render(
			() => (
				<TestContext.Provider value={element() ?? createMockHandlerProps()}>
					<Stats<HandlerProps> context={TestContext} getElement={() => element()} />
				</TestContext.Provider>
			),
			container,
		);

		let wrapper = container.querySelector(".stats__wrapper");
		expect(wrapper).toBeFalsy();

		await new Promise((resolve) => setTimeout(resolve, 100));
		setElement(createMockHandlerProps());

		await new Promise((resolve) => setTimeout(resolve, 100));
		wrapper = container.querySelector(".stats__wrapper");
		expect(wrapper).toBeTruthy();
	});

	it("rerenders when getElement function returns different values", async () => {
		const TestContext = createContext<string>("test");
		const [key, setKey] = createSignal<"element1" | "element2">("element1");
		const element1 = createMockHandlerProps();
		const element2 = createMockHandlerProps();

		const getElement = (_ctx: string) => {
			return key() === "element1" ? element1 : element2;
		};

		render(
			() => (
				<TestContext.Provider value="test">
					<Stats<string> context={TestContext} getElement={getElement} />
				</TestContext.Provider>
			),
			container,
		);

		let wrapper = container.querySelector(".stats__wrapper");
		expect(wrapper).toBeTruthy();

		await new Promise((resolve) => setTimeout(resolve, 100));
		setKey("element2");

		await new Promise((resolve) => setTimeout(resolve, 100));
		wrapper = container.querySelector(".stats__wrapper");
		expect(wrapper).toBeTruthy();
	});
});
