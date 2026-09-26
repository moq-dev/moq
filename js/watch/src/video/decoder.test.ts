import { describe, expect, it } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { Sync } from "../sync";
import { Decoder } from "./decoder";
import { Source } from "./source";

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

async function settle(): Promise<void> {
	for (let i = 0; i < 8; i++) await flush();
}

function config(fields: Record<string, unknown>): Catalog.VideoConfig {
	return Catalog.VideoConfigSchema.parse({
		codec: "avc1.640028",
		container: { kind: "legacy" },
		...fields,
	});
}

// The subscription is already closed, so the track never reaches VideoDecoder.
function closedConsumer(): Moq.Broadcast.Consumer {
	return { closed: new Signal("ended") } as unknown as Moq.Broadcast.Consumer;
}

function watchBroadcast(catalog: Signal<Catalog.Root>): Broadcast {
	return {
		out: { catalog },
		relativeBroadcast: () => closedConsumer(),
	} as unknown as Broadcast;
}

async function play(catalog: Signal<Catalog.Root>): Promise<{
	broadcast: Signal<Broadcast | undefined>;
	source: Source;
	sync: Sync;
	decoder: Decoder;
}> {
	const broadcast = new Signal<Broadcast | undefined>(watchBroadcast(catalog));
	const source = new Source({
		broadcast,
		supported: async () => true,
	});
	const sync = new Sync({ delay: Time.Milli(0) });
	await settle();
	const decoder = new Decoder({ source, sync });
	await settle();
	return { broadcast, source, sync, decoder };
}

describe("Decoder jitter across a source switch", () => {
	it("keeps the outgoing floor when the next source reuses the track name", async () => {
		const outgoing = new Signal<Catalog.Root>({
			video: { renditions: { video: config({ delay: 200, jitter: 60 }) } },
		});
		const { broadcast, source, sync, decoder } = await play(outgoing);
		try {
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(260));

			// A rise on the catalog this rendition subscribed to still resizes the floor.
			outgoing.set({
				video: { renditions: { video: config({ delay: 300, jitter: 60 }) } },
			});
			await settle();
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(360));

			broadcast.set(
				watchBroadcast(
					new Signal<Catalog.Root>({
						video: { renditions: { video: config({ jitter: 20 }) } },
					}),
				),
			);
			await settle();
			// The old frames are still on screen. The new catalog's same name is not their floor.
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(360));
		} finally {
			decoder.close();
			source.close();
			sync.close();
		}
	});

	it("follows a newer pending catalog when the track name stays the same", async () => {
		const outgoing = new Signal<Catalog.Root>({
			video: { renditions: { video: config({ jitter: 20 }) } },
		});
		const { broadcast, source, sync, decoder } = await play(outgoing);
		try {
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(20));

			broadcast.set(
				watchBroadcast(
					new Signal<Catalog.Root>({
						video: { renditions: { video: config({ delay: 200 }) } },
					}),
				),
			);
			await settle();
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(200));

			const newest = new Signal<Catalog.Root>({
				video: { renditions: { video: config({ delay: 500 }) } },
			});
			broadcast.set(watchBroadcast(newest));
			await settle();
			// The first switch is still pending, so the name did not change. The new catalog still has to win.
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(500));

			newest.set({
				video: { renditions: { video: config({ delay: 700 }) } },
			});
			await settle();
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(700));
		} finally {
			decoder.close();
			source.close();
			sync.close();
		}
	});

	it("keeps the outgoing floor when the next source uses a different name", async () => {
		const outgoing = new Signal<Catalog.Root>({
			video: { renditions: { hd: config({ delay: 200, jitter: 60 }) } },
		});
		const { broadcast, source, sync, decoder } = await play(outgoing);
		try {
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(260));

			broadcast.set(
				watchBroadcast(
					new Signal<Catalog.Root>({
						video: { renditions: { sd: config({ jitter: 20 }) } },
					}),
				),
			);
			await settle();
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(260));
		} finally {
			decoder.close();
			source.close();
			sync.close();
		}
	});
});
