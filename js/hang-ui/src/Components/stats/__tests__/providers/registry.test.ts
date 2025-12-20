import { describe, expect, it } from "vitest";
import { AudioProvider } from "../../providers/audio";
import { BufferProvider } from "../../providers/buffer";
import { NetworkProvider } from "../../providers/network";
import { getStatsInformationProvider, providers } from "../../providers/registry";
import { VideoProvider } from "../../providers/video";
import type { KnownStatsProviders } from "../../types";

describe("Registry", () => {
	it("should have all required providers registered", () => {
		const expectedProviders: KnownStatsProviders[] = ["network", "video", "audio", "buffer"];

		for (const statProvider of expectedProviders) {
			expect(providers[statProvider]).toBeDefined();
		}
	});

	it("should map video icon to VideoProvider", () => {
		expect(providers.video).toBe(VideoProvider);
	});

	it("should map audio icon to AudioProvider", () => {
		expect(providers.audio).toBe(AudioProvider);
	});

	it("should map buffer icon to BufferProvider", () => {
		expect(providers.buffer).toBe(BufferProvider);
	});

	it("should map network icon to NetworkProvider", () => {
		expect(providers.network).toBe(NetworkProvider);
	});

	it("should return correct provider class with getStatsInformationProvider", () => {
		expect(getStatsInformationProvider("video")).toBe(VideoProvider);
		expect(getStatsInformationProvider("audio")).toBe(AudioProvider);
		expect(getStatsInformationProvider("buffer")).toBe(BufferProvider);
		expect(getStatsInformationProvider("network")).toBe(NetworkProvider);
	});

	it("should return undefined for unknown icon", () => {
		expect(getStatsInformationProvider("unknown" as KnownStatsProviders)).toBeUndefined();
	});

	it("should instantiate providers correctly", () => {
		const providersList: KnownStatsProviders[] = ["network", "video", "audio", "buffer"];

		for (const statProvider of providersList) {
			const ProviderClass = getStatsInformationProvider(statProvider);
			expect(ProviderClass).toBeDefined();

			if (ProviderClass) {
				const instance = new ProviderClass({});
				expect(instance).toBeDefined();
				expect(instance.setup).toBeDefined();
				expect(instance.cleanup).toBeDefined();
			}
		}
	});
});
