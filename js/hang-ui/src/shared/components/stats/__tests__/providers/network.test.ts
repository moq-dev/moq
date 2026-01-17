import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { NetworkProvider } from "../../providers/network";
import type { ProviderContext, ProviderProps } from "../../types";

interface MockConnection {
	effectiveType?: "slow-2g" | "2g" | "3g" | "4g";
	downlinkMax?: number;
	downlink?: number;
	rtt?: number;
	saveData?: boolean;
	addEventListener?: (type: string, listener: () => void) => void;
	removeEventListener?: (type: string, listener: () => void) => void;
}

const mockNavigator: { onLine: boolean; connection?: MockConnection } = {
	onLine: true,
	connection: undefined,
};

describe("NetworkProvider", () => {
	let provider: NetworkProvider;
	let context: ProviderContext;
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
		provider?.cleanup();
		vi.useRealTimers();
		vi.clearAllMocks();
	});

	it("should display N/A initially when no connection info", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.connection = undefined;
		mockNavigator.onLine = true;

		provider.setup(context);
		expect(setDisplayData).toHaveBeenCalledWith("N/A");
	});

	it("should display offline status when browser is offline", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.onLine = false;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
		};

		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("offline");
	});

	it("should display effective connection type", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
		};

		provider.setup(context);

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
			const props: ProviderProps = {};
			provider = new NetworkProvider(props);
			mockNavigator.onLine = true;
			mockNavigator.connection = { effectiveType: type };

			provider.setup(context);

			expect(setDisplayData).toHaveBeenCalledWith(expected);
			provider.cleanup();
		}
	});

	it("should display bandwidth in Gbps", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			downlink: 5000,
		};

		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G\n5.0Gbps");
	});

	it("should display bandwidth in Mbps", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			downlink: 50,
		};

		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G\n50.0Mbps");
	});

	it("should display bandwidth in Kbps", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "3g" as const,
			downlink: 0.5,
		};

		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("3G\n500Kbps");
	});

	it("should display latency in milliseconds", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			rtt: 50,
		};

		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G\n50ms");
	});

	it("should display save-data status when enabled", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			saveData: true,
		};

		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G\nSave-Data");
	});

	it("should combine all network metrics", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			downlink: 50,
			rtt: 45,
			saveData: false,
		};

		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G\n50.0Mbps\n45ms");
	});

	it("should update display on online event", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			addEventListener: vi.fn(),
			removeEventListener: vi.fn(),
		};

		provider.setup(context);
		expect(setDisplayData).toHaveBeenLastCalledWith("4G");

		setDisplayData.mockClear();
		mockNavigator.onLine = false;

		window.dispatchEvent(new Event("offline"));

		expect(setDisplayData).toHaveBeenCalledWith("offline");
	});

	it("should update display on offline event", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = { effectiveType: "4g" as const };

		provider.setup(context);
		expect(setDisplayData).toHaveBeenLastCalledWith("4G");

		setDisplayData.mockClear();
		mockNavigator.onLine = false;

		window.dispatchEvent(new Event("offline"));

		expect(setDisplayData).toHaveBeenCalledWith("offline");
	});

	it("should ignore zero or negative bandwidth", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			downlink: 0,
		};

		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G");
	});

	it("should ignore zero or negative latency", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			rtt: 0,
		};

		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("4G");
	});

	it("should cleanup event listeners", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = { effectiveType: "4g" as const };

		provider.setup(context);

		const removeEventListenerSpy = vi.spyOn(window, "removeEventListener");
		provider.cleanup();

		expect(removeEventListenerSpy).toHaveBeenCalledWith("online", expect.any(Function));
		expect(removeEventListenerSpy).toHaveBeenCalledWith("offline", expect.any(Function));
	});

	it("should update periodically", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
		mockNavigator.onLine = true;
		mockNavigator.connection = {
			effectiveType: "4g" as const,
			downlink: 50,
		};

		provider.setup(context);
		setDisplayData.mockClear();

		vi.advanceTimersByTime(100);

		expect(setDisplayData).toHaveBeenCalled();
	});

	it("should prefer mozilla and webkit connection fallbacks", () => {
		const props: ProviderProps = {};
		provider = new NetworkProvider(props);
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

		provider.setup(context);

		expect(setDisplayData).toHaveBeenCalledWith("3G");
	});
});
