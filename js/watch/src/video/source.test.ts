import { describe, expect, it } from "bun:test";
import type * as Catalog from "@moq/hang/catalog";
import { Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { Source } from "./source";

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

async function settle(): Promise<void> {
	for (let i = 0; i < 5; i++) await flush();
}

function config(codec: string): Catalog.VideoConfig {
	return { codec, container: { kind: "legacy" } };
}

function broadcast(renditions: Record<string, Catalog.VideoConfig>): Broadcast {
	return {
		in: {
			connection: new Signal(undefined),
		},
		out: {
			catalog: new Signal({ video: { renditions } }),
		},
	} as unknown as Broadcast;
}

async function withoutWarnings(fn: () => Promise<void>): Promise<void> {
	const warn = console.warn;
	console.warn = () => {};
	try {
		await fn();
	} finally {
		console.warn = warn;
	}
}

describe("Source error signal", () => {
	it("is unsupported when the catalog has video renditions but none are supported", async () => {
		await withoutWarnings(async () => {
			const source = new Source({
				broadcast: broadcast({ hd: config("hev1.1.6.L120.90") }),
				supported: async () => false,
			});

			await settle();
			expect(source.out.error.peek()).toBe("unsupported");
			expect(source.out.available.peek()).toEqual({});

			source.close();
		});
	});

	it("treats a support probe throw as unsupported without aborting the remaining renditions", async () => {
		await withoutWarnings(async () => {
			const source = new Source({
				broadcast: broadcast({
					bad: config("not-a-codec"),
					good: config("avc1.640028"),
				}),
				supported: async (rendition) => {
					if (rendition.codec === "not-a-codec") throw new Error("probe failed");
					return true;
				},
			});

			await settle();
			expect(source.out.error.peek()).toBeUndefined();
			expect(Object.keys(source.out.available.peek())).toEqual(["good"]);

			source.close();
		});
	});

	it("is undefined when the catalog has no video renditions", async () => {
		const source = new Source({
			broadcast: broadcast({}),
			supported: async () => false,
		});

		await settle();
		expect(source.out.error.peek()).toBeUndefined();
		expect(source.out.available.peek()).toEqual({});

		source.close();
	});
});
