// Catalog-update costs with a warmed audio support verdict. Native probes run in browsers;
// Bun's stub decoder, a promise probe, isolates selection overhead and probe counts for the nightly series.
import type * as Catalog from "@moq/hang/catalog";
import { Signal } from "@moq/signals";
import { Source } from "../src/audio/source";
import type { Broadcast } from "../src/broadcast";
import { stubSupport } from "../src/webcodecs.test-util";

/** Measure metadata updates over rendition count, checking every published config. */
export async function run(): Promise<void> {
	const native = typeof AudioDecoder !== "undefined";
	using _stub = native ? undefined : stubSupport({ audio: () => true });
	// Count the decoder's probes by wrapping whichever one answers.
	let probes = 0;
	const isConfigSupported = AudioDecoder.isConfigSupported.bind(AudioDecoder);
	AudioDecoder.isConfigSupported = (config) => {
		probes++;
		return isConfigSupported(config);
	};
	console.log("probe,renditions,updates,probes,us_per_update");
	for (const count of [1, 8, 32]) {
		const renditions: Record<string, Catalog.AudioConfig> = Object.fromEntries(
			Array.from({ length: count }, (_, i) => [
				`audio${i}`,
				{
					codec: "opus",
					container: { kind: "legacy" },
					sampleRate: 48_000,
					numberOfChannels: 2,
				} as Catalog.AudioConfig,
			]),
		);
		const catalog = new Signal<Catalog.Root>({ audio: { renditions } });
		const source = new Source({ broadcast: { out: { catalog } } as unknown as Broadcast });
		try {
			await source.out.available.changed();
			if (Object.keys(source.out.available.peek()).length !== count) throw new Error("unsupported fixture");
			for (const [measured, updates] of [
				[false, 100],
				[true, 20],
				[true, 200],
			] as const) {
				const before = probes;
				const start = performance.now();
				for (let update = 0; update < updates; update++) {
					const enabled = update % 2 === 0;
					const changed = source.out.available.changed();
					catalog.set({
						audio: {
							renditions: Object.fromEntries(
								Object.entries(renditions).map(([name, c]) => [name, { ...c, enabled }]),
							),
						},
					});
					await changed;
					const available = Object.values(source.out.available.peek());
					if (available.length !== count || available.some((c) => c.enabled !== enabled))
						throw new Error("stale rendition metadata");
				}
				const us = ((performance.now() - start) * 1000) / updates;
				if (!measured) continue;
				console.log(`${native ? "native" : "promise"},${count},${updates},${probes - before},${us.toFixed(3)}`);
			}
		} finally {
			source.close();
		}
	}
}

await run();
