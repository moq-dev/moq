import { expect, test } from "bun:test";
import type * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import { Track } from "@moq/net";
import { Effect } from "@moq/signals";
import { CatalogProducer } from "./catalog.ts";

test("catalog producer seeds subscribers and fans out edits", async () => {
	const catalog = new CatalogProducer();

	// Edit before anyone subscribes: the value is retained, not lost.
	{
		using c = catalog.lock();
		c.value.video = { renditions: {} };
	}

	const effect = new Effect();
	const track = new Track("catalog.json");
	catalog.serve(track, effect);
	const consumer = new Json.Consumer<Catalog.Root>(track);

	// A new subscriber is seeded with the current catalog.
	expect((await consumer.next())?.video).toEqual({ renditions: {} });

	// An extension owner adds its own section; the subscriber sees the update, video untouched.
	{
		using c = catalog.lock();
		c.value.scte35 = { splices: [] };
	}
	const update = await consumer.next();
	expect(update?.video).toEqual({ renditions: {} });
	expect(update?.scte35).toEqual({ splices: [] });

	effect.close();
});

test("a reconnecting subscriber is seeded with the full current catalog", async () => {
	const catalog = new CatalogProducer();
	{
		using c = catalog.lock();
		c.value.video = { renditions: {} };
		c.value.scte35 = { splices: [] };
	}

	// The first subscription drains and ends...
	const first = new Effect();
	catalog.serve(new Track("catalog.json"), first);
	first.close();

	// ...and a fresh subscription still gets the current catalog, not nothing.
	const effect = new Effect();
	const track = new Track("catalog.json");
	catalog.serve(track, effect);
	const seeded = await new Json.Consumer<Catalog.Root>(track).next();
	expect(seeded?.video).toEqual({ renditions: {} });
	expect(seeded?.scte35).toEqual({ splices: [] });

	effect.close();
});
