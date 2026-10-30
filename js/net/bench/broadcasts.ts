/** Sweep observers against routes for one route update, and against tracks for one listing change. */

import { Signal } from "@moq/signals";
import type { Grant } from "../src/auth.ts";
import { Producer as BroadcastProducer } from "../src/broadcast.ts";
import { BroadcastCache } from "../src/consume.ts";
import * as Epoch from "../src/epoch.ts";
import { HopSchema, Route } from "../src/hop.ts";
import { hooks } from "../src/internal.ts";
import { AnnounceRequest } from "../src/lite/announce.ts";
import { Publisher } from "../src/lite/publisher.ts";
import { Subscriber } from "../src/lite/subscriber.ts";
import { TrackInfo } from "../src/lite/track.ts";
import { ALPN_07_WIP, Version } from "../src/lite/version.ts";
import { createMockTransportPair } from "../src/mock.ts";
import { Producer } from "../src/origin.ts";
import * as Path from "../src/path.ts";
import { Stream, Writer } from "../src/stream.ts";

// Keep protocol diagnostics out of the measured CSV on stdout.
console.debug = console.error;

const routeCounts = [8, 32, 128];
const observerCounts = [1, 8, 32];
const updates = 32;
let checksum = 0;

console.log("scope,routes,observers,update_us");
for (const scope of [
	"unscoped",
	"distinct-scopes",
	"producer-scopes",
	"rooted-producers",
	"full-name",
	"overlap-only",
] as const) {
	for (const routeCount of routeCounts) {
		for (const observerCount of observerCounts) {
			const origin = new Producer();
			const handles = Array.from({ length: routeCount }, (_, index) =>
				origin.requests(Path.from(`room/${index}${scope === "full-name" ? ".chat" : ""}`)),
			);
			const touched = Path.from(
				scope === "rooted-producers" ? "0" : scope === "full-name" ? "room/0.chat" : "room/0",
			);
			let notifications = 0;
			const streams = Array.from({ length: observerCount }, (_, index) =>
				scope === "full-name" || scope === "overlap-only"
					? origin.consume().announced(Path.Pattern.parse("**/*.chat"))
					: (scope === "producer-scopes"
							? origin.scope(
									Path.empty(),
									new Path.Patterns([Path.Pattern.parse(`room/**/tag-${index}`)]),
								)
							: scope === "rooted-producers"
								? origin.scope(
										Path.from("room"),
										new Path.Patterns([Path.Pattern.parse(`**/tag-${index}`)]),
									)
								: origin
						).announced(
							scope === "distinct-scopes" ? Path.Pattern.parse(`room/**/tag-${index}`) : undefined,
							// These scopes only overlap `room/<index>`, so they measure the covering view.
							{ covering: scope !== "unscoped" },
						),
			);
			const readers = streams.map(async (stream) => {
				for await (const event of stream) {
					if (event.kind !== "update") continue;
					if (event.prefix !== touched) throw new Error("unexpected touched route");
					notifications++;
					checksum++;
				}
			});
			await new Promise<void>((resolve) => setImmediate(resolve));

			const start = performance.now();
			for (let index = 0; index < updates; index++) {
				handles[0].updateRoute({ cost: BigInt(index + 1) });
				await new Promise<void>((resolve) => setImmediate(resolve));
			}
			const elapsed = performance.now() - start;
			const expected = scope === "overlap-only" ? 0 : updates * observerCount;
			if (notifications !== expected) {
				throw new Error(`expected ${expected} notifications, got ${notifications}`);
			}
			console.log(`${scope},${routeCount},${observerCount},${((elapsed * 1000) / updates).toFixed(1)}`);
			for (const stream of streams) stream.close();
			await Promise.all(readers);
			for (const handle of handles) handle.close();
			origin.close();
		}
	}
}
if (checksum === 0) throw new Error("benchmark did no work");

// One created and removed track reaches every listing, however many tracks the broadcast holds.
const listed = 256;
console.log("tracks,listings,create_remove_and_read_us");
for (const trackCount of [8, 128, 1024]) {
	for (const listingCount of [1, 8, 32]) {
		const broadcast = new BroadcastProducer();
		for (let index = 0; index < trackCount; index++) broadcast.createTrack(`held-${index}`);
		const listings = Array.from({ length: listingCount }, () => broadcast.consume().tracks());
		// Past each listing's initial set and SYNCED.
		for (const listing of listings) {
			for (let index = 0; index <= trackCount; index++) await listing.next();
		}
		const start = performance.now();
		for (let index = 0; index < listed; index++) {
			broadcast.createTrack(`new-${index}`);
			for (const listing of listings) {
				const event = await listing.next();
				if (event?.kind !== "added" || event.name !== `new-${index}`) throw new Error("listing missed a track");
			}
			broadcast.removeTrack(`new-${index}`);
			for (const listing of listings) {
				if ((await listing.next())?.kind !== "removed") throw new Error("listing missed a removal");
				checksum++;
			}
		}
		console.log(`${trackCount},${listingCount},${(((performance.now() - start) * 1000) / listed).toFixed(1)}`);
		broadcast.close();
	}
}

// Resolve one touched path while both the table and that path's readers grow.
console.log("request,routes,readers,update_and_read_us");
const epoch = Epoch.parse("01900000-0000-7000-8000-000000000001");
for (const pinned of [false, true]) {
	for (const routeCount of routeCounts) {
		for (const readerCount of observerCounts) {
			const origin = new Producer();
			const reader = origin.consume();
			const options = pinned ? { epoch } : {};
			const publishers = Array.from({ length: routeCount }, (_, index) => {
				const sourcePublication = origin.createBroadcast(Path.from(`room/${index}`));
				sourcePublication.announce({ epoch });
				return sourcePublication;
			});
			const background = publishers.map((_, index) => reader.request(Path.from(`room/${index}`), options));
			const requests = Array.from({ length: readerCount }, () => reader.request(Path.from("room/0"), options));
			const start = performance.now();
			for (let index = 0; index < updates; index++) {
				publishers[0].announce({ epoch, cost: BigInt(index + 1) });
				await Promise.resolve();
				for (const request of requests) {
					if (!request.active.peek()) throw new Error("same-epoch request lost its front");
					checksum++;
				}
			}
			console.log(
				`${pinned ? "pinned" : "unpinned"},${routeCount},${readerCount},${(((performance.now() - start) * 1000) / updates).toFixed(1)}`,
			);
			for (const request of [...requests, ...background]) request.close();
			origin.close();
		}
	}
}

// Grant updates touch every live fetch, with pattern lookup as the independent table axis.
const info: Uint8Array[] = [];
const writer = new Writer(
	new WritableStream<Uint8Array>({
		write: (chunk) => {
			info.push(new Uint8Array(chunk));
		},
	}),
	Version.DRAFT_05,
);
await new TrackInfo({}).encode(writer, Version.DRAFT_05);
writer.close();
await writer.closed;
const quic = {
	createBidirectionalStream: async () => {
		let inbound!: ReadableStreamDefaultController<Uint8Array>;
		let first = true;
		return {
			readable: new ReadableStream<Uint8Array>({
				start: (controller) => {
					inbound = controller;
				},
			}),
			writable: new WritableStream<Uint8Array>({
				write: (chunk) => {
					if (!first) return;
					first = false;
					if (chunk[0] === 6) {
						for (const bytes of info) inbound.enqueue(bytes);
						inbound.close();
					} else if (chunk[0] === 3) inbound.enqueue(Uint8Array.of(0, 1, 42));
					else throw new Error("unexpected benchmark stream");
				},
			}),
		};
	},
} as unknown as WebTransport;
console.log("auth,patterns,fetches,grant_update_us");
for (const patternCount of [8, 32, 128]) {
	for (const fetchCount of [1, 8, 32]) {
		const subscribe = new Path.Patterns([
			Path.Pattern.subtree("room"),
			...Array.from({ length: patternCount - 1 }, (_, i) => Path.Pattern.literal(`other/${i}`)),
		]);
		const grant = new Signal<Grant | undefined>({ publish: new Path.Patterns(), subscribe });
		const subscriber = new Subscriber(
			quic,
			Version.DRAFT_05,
			HopSchema.parse(1n),
			undefined,
			undefined,
			undefined,
			grant,
		);
		const groups = await Promise.all(
			Array.from({ length: fetchCount }, (_, i) => subscriber.fetchGroup(Path.from(`room/${i}`), "video", 0)),
		);
		const start = performance.now();
		for (let i = 0; i < 1024; i++) {
			grant.set({ publish: new Path.Patterns(), subscribe, expires: i });
			await Promise.resolve();
		}
		console.log(`fetch,${patternCount},${fetchCount},${(((performance.now() - start) * 1000) / 1024).toFixed(2)}`);
		for (const group of groups) {
			if (group.closed.peek() !== undefined) throw new Error("allowed fetch ended");
			group.close();
		}
		subscriber.close();
	}
}

// Restart a served prefix while both its path count and readers per path grow.
console.log("restart,paths,readers,update_us");
const nextEpoch = Epoch.parse("01900000-0000-7000-8000-000000000002");
for (const pathCount of routeCounts) {
	for (const readerCount of observerCounts) {
		const origin = new Producer();
		const dynamic = origin.requestsRoute(Path.from("room"), { epoch });
		const pending = dynamic.requested();
		const requests = Array.from({ length: pathCount }, (_, index) =>
			Array.from({ length: readerCount }, () => origin.consume().request(Path.from(`room/${index}`))),
		).flat();
		const broadcasts = Array.from({ length: pathCount }, () => new BroadcastProducer());
		for (const broadcast of broadcasts) {
			const request = (await pending.next()).value;
			if (request) hooks.acceptRequest(request, broadcast);
		}
		await Promise.resolve();
		const before = requests.map((request) => request.active.peek());
		const start = performance.now();
		dynamic.updateRoute({ epoch: nextEpoch });
		await Promise.resolve();
		const elapsed = performance.now() - start;
		for (const [index, request] of requests.entries()) {
			if (!before[index] || request.active.peek() !== before[index]) throw new Error("Restart lost an answer");
			checksum++;
		}
		console.log(`restart,${pathCount},${readerCount},${(elapsed * 1000).toFixed(1)}`);
		for (const request of requests) request.close();
		await pending.return?.();
		for (const broadcast of broadcasts) broadcast.close();
		dynamic.close();
		origin.close();
	}
}

// Only a touched announcement and path should cost work, regardless of unrelated routes or consumes.
console.log("subscriber,announcements,consumers,update_and_consume_us");
for (const routeCount of routeCounts) {
	for (const consumerCount of [8, 32, 128]) {
		const cache = new BroadcastCache();
		const owners = Array.from({ length: routeCount }, () => ({}));
		for (let i = 0; i < routeCount; i++) cache.announce(Path.from(`pool/${i}`), owners[i], Route.default);
		const sources = Array.from({ length: consumerCount }, () => new BroadcastProducer());
		const held = sources.map((source, i) => {
			const path = Path.from(`pool/${i % routeCount}/job/${i}`);
			return cache.insert(path, cache.instance(path), source.consume());
		});
		const touched = Path.from("pool/0/job/0");
		const start = performance.now();
		for (let i = 0; i < 4096; i++) {
			cache.announce(Path.from("pool/0"), owners[0], { ...Route.default, cost: BigInt(i) });
			const shared = cache.get(touched, cache.instance(touched));
			if (!shared || shared.closed !== held[0].closed) throw new Error("update replaced the source");
			shared.close();
		}
		console.log(`cache,${routeCount},${consumerCount},${(((performance.now() - start) * 1000) / 4096).toFixed(2)}`);
		for (const consumer of held) consumer.close();
		for (const source of sources) source.close();
	}
}

// Switching a more-specific route on and off must recover all held fallback consumers.
console.log("alternation,announcements,consumers,select_us,duplicates");
for (const routeCount of routeCounts) {
	for (const consumerCount of [1, 8, 32]) {
		const cache = new BroadcastCache();
		const owners = Array.from({ length: routeCount }, () => ({}));
		for (let i = 0; i < routeCount; i++) cache.announce(Path.from(`pool/${i}`), owners[i], Route.default);
		const sources = Array.from({ length: consumerCount }, () => new BroadcastProducer());
		const paths = sources.map((_, i) => Path.from(`pool/0/job/${i}`));
		const held = sources.map((source, i) => cache.insert(paths[i], cache.instance(paths[i]), source.consume()));
		const specific = {};
		const prefix = Path.from("pool/0/job");
		const epoch = Epoch.mint();
		let duplicates = 0;
		const start = performance.now();
		for (let round = 0; round < 256; round++) {
			if (round % 2 === 0) cache.announce(prefix, specific, { ...Route.default, epoch });
			else cache.withdraw(prefix, specific);
			for (let i = 0; i < consumerCount; i++) {
				const instance = cache.instance(paths[i]);
				const shared = cache.get(paths[i], instance);
				if (shared) shared.close();
				else {
					if (round > 0) duplicates++;
					held.push(cache.insert(paths[i], instance, sources[i].consume()));
				}
			}
		}
		console.log(
			`alternation,${routeCount},${consumerCount},${(((performance.now() - start) * 1000) / 256).toFixed(2)},${duplicates}`,
		);
		for (const consumer of held) consumer.close();
		for (const source of sources) source.close();
	}
}

// Blind invalidation touches only the held paths covered by the new announcement.
console.log("blind,announcements,consumers,invalidate_us,stale");
for (const routeCount of routeCounts) {
	for (const consumerCount of [1, 8, 32]) {
		const cache = new BroadcastCache();
		for (let i = 0; i < routeCount; i++) cache.announce(Path.from(`other/${i}`), {}, Route.default);
		const source = new BroadcastProducer();
		const keeper = source.consume();
		const paths = Array.from({ length: consumerCount }, (_, i) => Path.from(`pool/job/${i}`));
		const prefix = Path.from("pool");
		const owner = {};
		let elapsed = 0;
		let stale = 0;
		for (let round = 0; round < 256; round++) {
			const held = paths.map((path) => cache.insert(path, cache.instance(path), source.consume()));
			const start = performance.now();
			cache.announce(prefix, owner, Route.default);
			cache.withdraw(prefix, owner);
			elapsed += performance.now() - start;
			for (const path of paths) {
				const shared = cache.get(path, cache.instance(path));
				if (shared) {
					stale++;
					shared.close();
				}
			}
			for (const consumer of held) consumer.close();
		}
		console.log(`blind,${routeCount},${consumerCount},${((elapsed * 1000) / 256).toFixed(2)},${stale}`);
		keeper.close();
		source.close();
	}
}

// Retiring a covering route cancels its pending descendants, not unrelated consumers.
console.log("retirement,routes,consumers,retire_us,refused");
for (const routeCount of routeCounts) {
	for (const consumerCount of observerCounts) {
		let elapsed = 0;
		let refused = 0;
		for (let round = 0; round < 256; round++) {
			const cache = new BroadcastCache();
			const source = new BroadcastProducer();
			const owners = Array.from({ length: routeCount }, () => ({}));
			const held = [];
			for (let route = 0; route < routeCount; route++) {
				const prefix = Path.from(`route/${route}`);
				cache.announce(prefix, owners[route], Route.default);
				for (let consumer = 0; consumer < (route === 0 ? consumerCount : 1); consumer++) {
					const path = Path.from(`${prefix}/job/${consumer}`);
					const instance = cache.instance(path);
					if (route === 0)
						instance.validity?.signal.addEventListener(
							"abort",
							() => {
								refused++;
							},
							{ once: true },
						);
					held.push(cache.insert(path, instance, source.consume()));
				}
			}
			const start = performance.now();
			cache.withdraw(Path.from("route/0"), owners[0]);
			elapsed += performance.now() - start;
			for (const consumer of held) consumer.close();
			source.close();
		}
		console.log(`retirement,${routeCount},${consumerCount},${((elapsed * 1000) / 256).toFixed(2)},${refused}`);
	}
}

// Exercise each sender's keyed queue while its first wire write is stalled.
console.log("scope,routes,observers,update_us");
for (const routeCount of routeCounts) {
	for (const observerCount of observerCounts) {
		const origin = new Producer();
		const routes = Array.from({ length: routeCount }, (_, index) => {
			const broadcastPublication = origin.createBroadcast(Path.from(`z/${index}`));
			const broadcast = broadcastPublication.broadcast;
			broadcastPublication.announce();
			return broadcast;
		});
		const senders = Array.from({ length: observerCount }, () => {
			const pair = createMockTransportPair(ALPN_07_WIP);
			const publisher = new Publisher(pair.server, Version.DRAFT_07, HopSchema.parse(1n), origin.consume());
			const entered = Promise.withResolvers<void>();
			const release = Promise.withResolvers<void>();
			const stream = new Stream({
				version: Version.DRAFT_07,
				readable: new ReadableStream<Uint8Array>(),
				writable: new WritableStream<Uint8Array>({
					async write() {
						entered.resolve();
						await release.promise;
					},
				}),
			});
			const running = publisher.runAnnounce(new AnnounceRequest(Path.empty()), stream);
			return { pair, publisher, entered, release, running };
		});
		await Promise.all(senders.map((sender) => sender.entered.promise));
		const start = performance.now();
		for (let index = 0; index < updates; index++) {
			const broadcastPublication = origin.createBroadcast(Path.from(`a/${index}`));
			const broadcast = broadcastPublication.broadcast;
			broadcastPublication.announce();
			await new Promise<void>((resolve) => setImmediate(resolve));
			broadcast.close();
			await new Promise<void>((resolve) => setImmediate(resolve));
		}
		console.log(
			`stalled-announce,${routeCount},${observerCount},${(((performance.now() - start) * 1000) / updates).toFixed(1)}`,
		);
		await Promise.all(
			senders.map(async ({ pair, publisher, release, running }) => {
				const drained = publisher.drain();
				release.resolve();
				await drained;
				await running;
				publisher.close();
				pair.client.close();
				pair.server.close();
			}),
		);
		for (const route of routes) route.close();
		origin.close();
	}
}
