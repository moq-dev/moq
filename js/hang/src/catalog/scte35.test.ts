import { expect, test } from "bun:test";
import * as Json from "@moq/json";
import { Track } from "@moq/net";
import * as z from "zod/mini";
import { RootSchema } from "./root.ts";
import { Scte35Schema } from "./scte35.ts";

// Stress-test the extension API end to end: an application extends the base catalog with the
// scte35 section, then exports and imports it through the same producer/consumer the catalog uses.
const AppSchema = z.extend(RootSchema, { scte35: z.optional(Scte35Schema) });
type App = z.infer<typeof AppSchema>;

test("scte35 section round-trips through export/import", async () => {
	const track = new Track("catalog.json");
	const producer = new Json.Producer<App>(track, { schema: AppSchema, initial: {} });
	const consumer = new Json.Consumer<App>(track, { schema: AppSchema });

	// Export: the base owner publishes the media section...
	producer.mutate((catalog) => {
		catalog.video = { renditions: {} };
	});
	expect((await consumer.next())?.video).toEqual({ renditions: {} });

	// ...and the scte35 owner adds its own section via the shared producer, without clobbering video.
	producer.mutate((catalog) => {
		catalog.scte35 = { splices: [{ id: 1, out: true, startTime: 10 }] };
	});

	// Import: the consumer reconstructs the full catalog, validated against the extended schema.
	const imported = await consumer.next();
	expect(imported?.video).toEqual({ renditions: {} });
	expect(imported?.scte35?.splices).toEqual([{ id: 1, out: true, startTime: 10 }]);
});

test("removing the scte35 section is observable on import", async () => {
	const track = new Track("catalog.json");
	const producer = new Json.Producer<App>(track, { schema: AppSchema, initial: {} });
	const consumer = new Json.Consumer<App>(track, { schema: AppSchema });

	producer.mutate((catalog) => {
		catalog.video = { renditions: {} };
	});
	expect((await consumer.next())?.video).toEqual({ renditions: {} });

	producer.mutate((catalog) => {
		catalog.scte35 = { splices: [] };
	});
	expect((await consumer.next())?.scte35).toEqual({ splices: [] });

	// Clearing the section drops it from the catalog; the media section is untouched.
	producer.mutate((catalog) => {
		delete catalog.scte35;
	});
	const after = await consumer.next();
	expect(after?.scte35).toBeUndefined();
	expect(after?.video).toEqual({ renditions: {} });
});
