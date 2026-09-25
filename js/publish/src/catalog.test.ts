import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import { Track } from "@moq/net";
import { Effect } from "@moq/signals";
import { CatalogProducer } from "./catalog.ts";

test("catalog producer seeds subscribers and fans out edits", async () => {
	const catalog = new CatalogProducer();

	// Edit before anyone subscribes: the value is retained, not lost.
	catalog.mutate((c) => {
		c.video = { renditions: {} };
	});

	const effect = new Effect();
	const track = new Track.Producer("catalog.json");
	catalog.serve(track, effect);
	const consumer = new Json.Snapshot.Consumer<Catalog.Root>({ track: track.subscribe() });

	// A new subscriber is seeded with the current catalog.
	expect((await consumer.next())?.video).toEqual({ renditions: {} });

	// An extension owner adds its own section; the subscriber sees the update, video untouched.
	catalog.mutate((c) => {
		c.scte35 = { splices: [] };
	});
	const update = await consumer.next();
	expect(update?.video).toEqual({ renditions: {} });
	expect(update?.scte35).toEqual({ splices: [] });

	effect.close();
});

test("catalog producer publishes every update as a snapshot group", async () => {
	const catalog = new CatalogProducer();
	catalog.mutate((c) => {
		c.video = { renditions: {} };
	});

	const effect = new Effect();
	const track = new Track.Producer("catalog.json");
	catalog.serve(track, effect);
	const subscriber = track.subscribe().ordered();

	const first = await subscriber.nextGroup();
	expect(first?.sequence).toBe(0);
	expect(await first?.readJson()).toEqual({ clock: expect.anything(), video: { renditions: {} } });
	expect(first?.done).toBe(true);

	catalog.mutate((c) => {
		c.scte35 = { splices: [] };
	});

	const second = await subscriber.nextGroup();
	expect(second?.sequence).toBe(1);
	expect(await second?.readJson()).toEqual({
		clock: expect.anything(),
		video: { renditions: {} },
		scte35: { splices: [] },
	});
	expect(second?.done).toBe(true);

	effect.close();
});

test("catalog producer advertises the page clock from the first snapshot", async () => {
	const catalog = new CatalogProducer();

	const effect = new Effect();
	const track = new Track.Producer("catalog.json");
	catalog.serve(track, effect);
	const consumer = new Json.Snapshot.Consumer<Catalog.Root>({ track: track.subscribe() });

	// Before any rendition: a live-only publisher exposes its clock without an archive.
	const first = Catalog.RootSchema.parse(await consumer.next());
	if (!first.clock) throw new Error("expected a root clock");
	expect(first.archive).toBeUndefined();
	expect(first.clock.timescale).toBe(1_000_000);

	// A timestamp stamped the way capture does (performance.now() in microseconds) maps to now.
	const pts = Math.round(performance.now() * 1000);
	const wall = Catalog.wallClockTime(first.clock, pts, 1_000_000).getTime();
	expect(Math.abs(wall - Date.now())).toBeLessThan(50);

	// Later edits keep the mapping: it is fixed for the broadcast.
	catalog.mutate((c) => {
		c.video = { renditions: {} };
	});
	const second = Catalog.RootSchema.parse(await consumer.next());
	expect(second.clock).toEqual(first.clock);

	effect.close();
});

test("a reconnecting subscriber is seeded with the full current catalog", async () => {
	const catalog = new CatalogProducer();
	catalog.mutate((c) => {
		c.video = { renditions: {} };
		c.scte35 = { splices: [] };
	});

	// The first subscription drains and ends...
	const first = new Effect();
	catalog.serve(new Track.Producer("catalog.json"), first);
	first.close();

	// ...and a fresh subscription still gets the current catalog, not nothing.
	const effect = new Effect();
	const track = new Track.Producer("catalog.json");
	catalog.serve(track, effect);
	const seeded = await new Json.Snapshot.Consumer<Catalog.Root>({ track: track.subscribe() }).next();
	expect(seeded?.video).toEqual({ renditions: {} });
	expect(seeded?.scte35).toEqual({ splices: [] });

	effect.close();
});

test("catalog producer refuses zero jitter before retaining an edit", () => {
	const catalog = new CatalogProducer();
	for (const section of ["audio", "video"] as const) {
		expect(() =>
			catalog.mutate((value) => {
				Object.assign(value, {
					[section]: {
						renditions: {
							media: {
								codec: "opus",
								container: { kind: "legacy" },
								sampleRate: 48000,
								numberOfChannels: 2,
								jitter: 0,
							},
						},
					},
				});
			}),
		).toThrow("omit jitter");
	}
	catalog.mutate((value) => {
		expect(value.audio).toBeUndefined();
		expect(value.video).toBeUndefined();
	});
});

for (const section of ["audio", "video"] as const) {
	test(`catalog refuses ${section} jitter decreases without retaining them`, () => {
		const catalog = new CatalogProducer();
		catalog.mutate((value) => {
			Object.assign(value, {
				[section]: {
					renditions: {
						media: {
							codec: "opus",
							container: { kind: "legacy" },
							sampleRate: 48000,
							numberOfChannels: 2,
							jitter: 100,
						},
					},
				},
			});
		});

		// The section is optional on the loose root type, so re-read it through a guard.
		const retained = (value: Catalog.Root) => {
			const sectionValue = value[section];
			if (!sectionValue) throw new Error(`expected a retained ${section} section`);
			return sectionValue;
		};
		for (const jitter of [Catalog.u53(50), undefined]) {
			expect(() =>
				catalog.mutate((value) => {
					retained(value).renditions.media.jitter = jitter;
				}),
			).toThrow("jitter cannot decrease");
			catalog.mutate((value) => {
				expect(retained(value).renditions.media.jitter).toBe(Catalog.u53(100));
			});
		}
		catalog.mutate((value) => {
			delete retained(value).renditions.media;
		});
		catalog.mutate((value) => {
			Object.assign(retained(value).renditions, {
				media: {
					codec: "opus",
					container: { kind: "legacy" },
					sampleRate: 48000,
					numberOfChannels: 2,
					jitter: 50,
				},
			});
		});
	});
}
