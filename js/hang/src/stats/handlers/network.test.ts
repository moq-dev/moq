import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { NetworkHandler } from "./network";
import type { HandlerContext, HandlerProps } from "../types";

interface MockConnection {
	effectiveType?: "slow-2g" | "2g" | "3g" | "4g";
	downlinkMax?: number;
	downlink?: number;
	rtt?: number;
	saveData?: boolean;
}

const mockNavigator: { onLine: boolean; connection?: MockConnection } = {
	onLine: true,
	connection: undefined,
};

describe("NetworkHandler", () => {
	let handler: NetworkHandler;
	let context: HandlerContext;
	let setDisplayData: ReturnType<typeof vi.fn>;

	beforeEach(() => {
		setDisplayData = vi.fn();
		context = { setDisplayData };

		Object.defineProperty(window, "navigator", {
			value: mockNavigator,
			writable: true,
			configurable: true,
		});

		vi.useFakeTimers();
	});

	afterEach(() => {
		vi.useRealTimers();
		vi.clearAllMocks();
		handler.cleanup();
	});

	it("should display N/A initially when no connection info", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.connection = undefined;
		mockNavigator.onLine = true;

		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("N/A");
	});

	it("should display offline status when browser is offline", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = false;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
		};

		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("offline");
	});

	it("should display effective connection type", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
		};

		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G");
	});

	it("should map all effective connection types", () => {
		const effectiveTypes: Array<["slow-2g", string] | ["2g", string] | ["3g", string] | ["4g", string]> = [
			["slow-2g", "Slow-2G"],
			["2g", "2G"],
			["3g", "3G"],
			["4g", "4G"],
		];

		for (const [type, expected] of effectiveTypes) {
			setDisplayData.mockClear();
			const props: HandlerProps = {};
			handler = new NetworkHandler(props);
			mockNavigator.onLine = true;
			mockNavigator.connection = { effectiveType: type };

			handler.setup(context);

			expect(setDisplayData).toHaveBeenCalledWith(expected);
			handler.cleanup();
		}
	});

	it("should display bandwidth in Gbps", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			downlink: 5000,
		};

		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G\n5.0Gbps");
	});

	it("should display bandwidth in Mbps", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			downlink: 50,
		};

		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G\n50.0Mbps");
	});

	it("should display bandwidth in Kbps", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "3g" as const,
			downlink: 0.5,
		};

		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("3G\n500Kbps");
	});

	it("should display latency in milliseconds", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			rtt: 50,
		};

		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G\n50ms");
	});

	it("should display save-data status when enabled", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			saveData: true,
		};

		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G\nSave-Data");
	});

	it("should combine all network metrics", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			downlink: 50,
			rtt: 45,
			saveData: false,
		};

		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G\n50.0Mbps\n45ms");
	});

	it("should update display on online event", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = false;
		mockNavigator.connection = undefined;

		handler.setup(context);
		expect(setDisplayData).toHaveBeenLastCalledWith("offline");

		setDisplayData.mockClear();
		mockNavigator.onLine = true;
		mockNavigator.connection = { effectiveType: "4g" as const };

		vi.advanceTimersByTime(100);

		expect(setDisplayData).toHaveBeenCalled();
	});

	it("should update display on offline event", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = { effectiveType: "4g" as const };

		handler.setup(context);
		expect(setDisplayData).toHaveBeenLastCalledWith("4G");

		setDisplayData.mockClear();
		mockNavigator.onLine = false;

		window.dispatchEvent(new Event("offline"));

		expect(setDisplayData).toHaveBeenCalledWith("offline");
	});

	it("should ignore zero or negative bandwidth", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			downlink: 0,
		};

		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G");
	});

	it("should ignore zero or negative latency", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			rtt: 0,
		};

		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G");
	});

	it("should cleanup event listeners", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = { effectiveType: "4g" as const };

		handler.setup(context);

		const removeEventListenerSpy = vi.spyOn(window, "removeEventListener");
		handler.cleanup();

		expect(removeEventListenerSpy).toHaveBeenCalledWith("online", expect.any(Function));
		expect(removeEventListenerSpy).toHaveBeenCalledWith("offline", expect.any(Function));
	});

	it("should update periodically", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			downlink: 50,
		};

		handler.setup(context);
		setDisplayData.mockClear();

		vi.advanceTimersByTime(100);

		expect(setDisplayData).toHaveBeenCalled();
	});

	it("should prefer mozilla and webkit connection fallbacks", () => {
		const props: HandlerProps = {};
		handler = new NetworkHandler(props);
		mockNavigator.onLine = true;

		const mozConnection = {
			effectiveType: "3g" as const,
		};

		Object.defineProperty(window, "navigator", {
			value: {
				onLine: true,
				connection: undefined,
				mozConnection,
			},
			writable: true,
			configurable: true,
		});

		handler.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("3G");
	});
});
