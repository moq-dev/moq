/** Sweep tracks x viewers for 1000 estimate rises inside one second. */

import { Manual, Milli, Nano } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import { Track } from "@moq/net";
import { Effect } from "@moq/signals";
import { CatalogProducer } from "../src/catalog";

async function measure(tracks: number, viewers: number, clock: Manual) {
	const scope = new Effect();
	const catalog = new CatalogProducer();
	const media = Array.from({ length: tracks }, (_, index) => new Track.Producer(`v${index}`));
	catalog.mutate((root) => {
		root.video = {
			renditions: Object.fromEntries(
				media.map((track) => [
					track.name,
					{
						codec: "avc1.640028",
						container: { kind: "legacy" },
					},
				]),
			),
		};
	});
	const output = new Track.Producer("catalog.json");
	catalog.serve(output, scope);
	const subscribers = Array.from({ length: viewers }, () => {
		const subscriber = output.subscribe();
		return {
			subscriber,
			catalog: new Json.Snapshot.Consumer<Catalog.Root>({ track: subscriber }),
			media: media.map((track) => track.subscribe()),
		};
	});
	// Initial configuration is outside the estimate traffic being measured.
	for (const viewer of subscribers) await viewer.catalog.latest();
	let latest = subscribers[0].subscriber.latest();
	let publishes = 0;
	let updates = 0;
	const receive = async () => {
		const next = subscribers[0].subscriber.latest();
		if (next === latest) return;
		latest = next;
		publishes++;
		for (const viewer of subscribers) {
			const root = (await viewer.catalog.latest())?.value;
			for (let index = 0; index < media.length; index++) {
				// Model the player's changed latency floor with real subscription handles.
				viewer.media[index].update({
					maxDelay: Milli(root?.video?.renditions[media[index].name].jitter ?? 0),
				});
				updates++;
			}
		}
	};
	const start = performance.now();
	for (let step = 0; step < 1000; step++) {
		if (step > 0) clock.advance(Nano.fromMilli(Milli(1)));
		catalog.mutate((root) => {
			if (!root.video) throw new Error("video missing");
			for (const config of Object.values(root.video.renditions)) config.jitter = Catalog.u53(step + 1);
		});
		await receive();
	}
	await clock.advanceAsync(Nano.fromMilli(Milli(1)));
	await receive();
	const elapsed = performance.now() - start;
	if (publishes !== 2 || updates !== 2 * tracks * viewers) {
		throw new Error(`estimate churn regression: ${publishes} publishes, ${updates} subscribe updates`);
	}
	console.log(`${tracks},${viewers},${publishes},${updates},${1000 * tracks * viewers},${elapsed.toFixed(1)}`);
	scope.close();
	output.close();
	for (const viewer of subscribers) {
		viewer.subscriber.close();
		for (const track of viewer.media) track.close();
	}
	for (const track of media) track.close();
}

console.log("tracks,viewers,catalog_publishes,subscribe_updates,unlimited_subscribe_updates,elapsed_ms");
for (const tracks of [1, 4, 16]) {
	for (const viewers of [1, 4, 16]) {
		const clock = new Manual();
		const restore = clock.install();
		try {
			await measure(tracks, viewers, clock);
		} finally {
			restore();
		}
	}
}
