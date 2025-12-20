import { createContext, createSignal } from "solid-js";
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { Stats } from "../Stats";
import type { ProviderProps } from "../types";
import { createMockProviderProps } from "./utils";

describe("Stats Component", () => {
	let container: HTMLDivElement;
	let dispose: (() => void) | undefined;

	beforeEach(() => {
		container = document.createElement("div");
		document.body.appendChild(container);
	});

	afterEach(() => {
		dispose?.();
		dispose = undefined;
	});

	it("renders stats container", () => {
		const mockProps = createMockProviderProps();
		const TestContext = createContext<ProviderProps>(mockProps);

		dispose = render(
			() => (
				<TestContext.Provider value={mockProps}>
					<Stats<ProviderProps> context={TestContext} getElement={() => mockProps} />
				</TestContext.Provider>
			),
			container,
		);

		const stats = container.querySelector(".stats");
		expect(stats).toBeTruthy();
	});

	it("waits for audio and video before rendering", async () => {
		const mockDefault = createMockProviderProps();
		const TestContext = createContext<ProviderProps>(mockDefault);

		dispose = render(() => {
			const [mediaElement, setMediaElement] = createSignal<ProviderProps | undefined>(undefined);

			// Set media element after initial render
			setTimeout(() => setMediaElement(createMockProviderProps()), 100);

			return (
				<TestContext.Provider value={mediaElement() ?? createMockProviderProps()}>
					<Stats<ProviderProps> context={TestContext} getElement={() => mediaElement()} />
				</TestContext.Provider>
			);
		}, container);

		// Initially no StatsPanel should be rendered
		let panel = container.querySelector(".stats__panel");
		expect(panel).toBeFalsy();

		// Wait for setTimeout to trigger
		await new Promise((resolve) => setTimeout(resolve, 150));

		// Now panel should be rendered
		panel = container.querySelector(".stats__panel");
		expect(panel).toBeTruthy();
	});

	it("only renders when both audio and video are available", async () => {
		const mockDefault = createMockProviderProps();
		const TestContext = createContext<ProviderProps>(mockDefault);
		const mockWithoutVideo = createMockProviderProps({ video: false });

		dispose = render(() => {
			const [mediaElement, setMediaElement] = createSignal<Partial<ProviderProps> | undefined>(mockWithoutVideo);

			// Set full props after initial render
			setTimeout(() => setMediaElement(createMockProviderProps()), 100);

			return (
				<TestContext.Provider value={mediaElement() as ProviderProps}>
					<Stats<ProviderProps>
						context={TestContext}
						getElement={() => mediaElement() as ProviderProps | undefined}
					/>
				</TestContext.Provider>
			);
		}, container);

		let panel = container.querySelector(".stats__panel");

		// Wait for setTimeout to trigger
		await new Promise((resolve) => setTimeout(resolve, 150));

		panel = container.querySelector(".stats__panel");
		expect(panel).toBeTruthy();
	});

	it("works with different context types", () => {
		interface CustomContext {
			hangWatch: () => ProviderProps | undefined;
		}

		const mockProps = createMockProviderProps();
		const contextValue: CustomContext = {
			hangWatch: () => mockProps,
		};
		const CustomTestContext = createContext<CustomContext>(contextValue);

		dispose = render(
			() => (
				<CustomTestContext.Provider value={contextValue}>
					<Stats<CustomContext> context={CustomTestContext} getElement={(ctx) => ctx?.hangWatch()} />
				</CustomTestContext.Provider>
			),
			container,
		);

		const stats = container.querySelector(".stats");
		expect(stats).toBeTruthy();

		const panel = container.querySelector(".stats__panel");
		expect(panel).toBeTruthy();
	});

	it("applies styles inline", () => {
		const mockProps = createMockProviderProps();
		const TestContext = createContext<ProviderProps>(mockProps);

		dispose = render(
			() => (
				<TestContext.Provider value={mockProps}>
					<Stats<ProviderProps> context={TestContext} getElement={() => mockProps} />
				</TestContext.Provider>
			),
			container,
		);

		const style = container.querySelector(".stats style");
		expect(style).toBeTruthy();
	});

	it("provides context value to child components", () => {
		const mockProps = createMockProviderProps();
		const TestContext = createContext<ProviderProps>(mockProps);

		dispose = render(
			() => (
				<TestContext.Provider value={mockProps}>
					<Stats<ProviderProps> context={TestContext} getElement={() => mockProps} />
				</TestContext.Provider>
			),
			container,
		);

		const panel = container.querySelector(".stats__panel");
		expect(panel).toBeTruthy();
	});

	it("handles undefined context gracefully", () => {
		const mockDefault = createMockProviderProps();
		const TestContext = createContext<ProviderProps>(mockDefault);

		dispose = render(
			() => (
				<TestContext.Provider value={createMockProviderProps()}>
					<Stats<ProviderProps> context={TestContext} getElement={() => undefined} />
				</TestContext.Provider>
			),
			container,
		);

		// Should not render panel when element is undefined
		const panel = container.querySelector(".stats__panel");
		expect(panel).toBeFalsy();
	});

	it("updates when context changes", async () => {
		const mockDefault = createMockProviderProps();
		const TestContext = createContext<ProviderProps>(mockDefault);

		dispose = render(() => {
			const [element, setElement] = createSignal<ProviderProps | undefined>(undefined);

			// Set element after initial render
			setTimeout(() => setElement(createMockProviderProps()), 100);

			return (
				<TestContext.Provider value={element() ?? createMockProviderProps()}>
					<Stats<ProviderProps> context={TestContext} getElement={() => element()} />
				</TestContext.Provider>
			);
		}, container);

		let panel = container.querySelector(".stats__panel");
		expect(panel).toBeFalsy();

		// Wait for setTimeout to trigger
		await new Promise((resolve) => setTimeout(resolve, 150));

		panel = container.querySelector(".stats__panel");
		expect(panel).toBeTruthy();
	});

	it("rerenders when getElement function returns different values", async () => {
		const TestContext = createContext<string>("test");
		const element1 = createMockProviderProps();
		const element2 = createMockProviderProps();

		dispose = render(() => {
			const [key, setKey] = createSignal<"element1" | "element2">("element1");

			const getElement = (_ctx: string) => {
				return key() === "element1" ? element1 : element2;
			};

			// Change key after initial render
			setTimeout(() => setKey("element2"), 100);

			return (
				<TestContext.Provider value="test">
					<Stats<string> context={TestContext} getElement={getElement} />
				</TestContext.Provider>
			);
		}, container);

		let panel = container.querySelector(".stats__panel");
		expect(panel).toBeTruthy();

		// Wait for setTimeout to trigger
		await new Promise((resolve) => setTimeout(resolve, 150));

		panel = container.querySelector(".stats__panel");
		expect(panel).toBeTruthy();
	});
});
