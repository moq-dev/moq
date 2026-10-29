/**
 * Compressed-catalog interop reader: offer one version, read a hang catalog the way `@moq/watch`
 * does, and report whether the track arrived compressed. Only moq-lite-07 carries the flag; every
 * other version receives the relay's inflated copy.
 *
 *     node --import tsx catalog.ts URL BROADCAST VERSION
 *
 * @module
 */
import { strict as assert } from "node:assert";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";

install();
console.debug = console.error;
console.info = console.error;
const [url, path, version] = process.argv.slice(2);
if (!url || !path || !version) throw new Error("usage: catalog.ts URL BROADCAST VERSION");

const origin = new Moq.Origin.Producer();
const connection = await Moq.Connection.connect({
	url: new URL(url),
	websocket: { enabled: false },
	versions: [version],
	consume: origin,
});
try {
	const request = origin.follow(Moq.Path.from(path));
	try {
		let broadcast = request.active.peek();
		while (!broadcast) {
			await request.active.changed();
			broadcast = request.active.peek();
		}
		const track = broadcast.track(Catalog.TRACK).subscribe({ priority: Catalog.PRIORITY.catalog });
		try {
			const { compress } = await track.info();
			assert.equal(compress, version === "moq-lite-07-wip", "wrong compression metadata");
			const consumer = new Json.Snapshot.Consumer<Catalog.Root>({ track, schema: Catalog.RootSchema });
			const root = (await consumer.latest())?.value;
			assert(root, "catalog ended before a snapshot");
			const video = Object.keys(root.video?.renditions ?? {}).sort();
			const audio = Object.keys(root.audio?.renditions ?? {}).sort();
			console.error(`catalog compress=${compress} video=${video.join(",")} audio=${audio.join(",")}`);
		} finally {
			track.close();
		}
	} finally {
		request.close();
	}
} finally {
	connection.close();
	origin.close();
}
process.exit(0);
