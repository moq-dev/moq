import { beforeEach, describe, expect, it, vi } from "vitest";
import { BaseProvider } from "../../providers/base";
import type { ProviderContext, ProviderProps } from "../../types";

class TestProvider extends BaseProvider {
	public setupCalled = false;
	public setupContext: ProviderContext | undefined;
	public cleanupCalled = false;

	setup(context: ProviderContext): void {
		this.setupCalled = true;
		this.setupContext = context;
	}

	cleanup(): void {
		this.cleanupCalled = true;
		super.cleanup();
	}
}

describe("BaseProvider", () => {
	let provider: TestProvider;
	let context: ProviderContext;

	beforeEach(() => {
		const props: ProviderProps = {};
		provider = new TestProvider(props);
		context = {
			setDisplayData: vi.fn(),
		};
	});

	it("should initialize with props", () => {
		const props: ProviderProps = { audio: undefined, video: undefined };
		const testProvider = new TestProvider(props);
		expect(testProvider).toBeDefined();
	});

	it("should call setup method", () => {
		provider.setup(context);
		expect(provider.setupCalled).toBe(true);
		expect(provider.setupContext).toEqual(context);
	});

	it("should cleanup", () => {
		provider.setup(context);
		expect(provider.cleanupCalled).toBe(false);
		provider.cleanup();
		expect(provider.cleanupCalled).toBe(true);
		expect(provider.setupCalled).toBe(true);
	});

	it("should have setup and cleanup methods", () => {
		expect(provider.setup).toBeDefined();
		expect(provider.cleanup).toBeDefined();
		expect(typeof provider.setup).toBe("function");
		expect(typeof provider.cleanup).toBe("function");
	});
});
