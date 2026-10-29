/** Sweep request restart, cancellation, and discovery settlement over independent origin and waiter dimensions. */
import { Producer as Broadcast } from "../src/broadcast";
import * as Epoch from "../src/epoch";
import { Publisher as IetfPublisher } from "../src/ietf/publisher";
import { RequestOk } from "../src/ietf/request";
import { NativeSession } from "../src/ietf/session";
import {
	SubscribeNamespace,
	SubscribeNamespaceEntry,
	SubscribeNamespaceEntryDone,
} from "../src/ietf/subscribe_namespace";
import { ALPN, Version } from "../src/ietf/version";
import { hooks } from "../src/internal";
import { createMockTransportPair } from "../src/mock";
import { Producer } from "../src/origin";
import * as Path from "../src/path";
import { readsFrom } from "../src/resume.test-util";
import { Stream } from "../src/stream";
import { wireOf } from "../src/wire";

// Keep protocol diagnostics out of the measured CSV on stdout.
console.debug = console.error;

const settle = () => new Promise<void>((resolve) => setImmediate(resolve));
console.log("touched,routes,followers,update_us");
for (const touched of ["watched", "unrelated"])
	for (const routes of [8, 32, 128])
		for (const followers of [1, 8, 32]) {
			const origin = new Producer();
			const path = Path.from("room/0");
			const broadcasts = Array.from({ length: routes }, (_, i) => {
				const broadcastPublication = origin.createBroadcast(Path.from(`room/${i}`));
				const broadcast = broadcastPublication.broadcast;
				broadcastPublication.announce();
				return broadcast;
			});
			const requests = Array.from({ length: followers }, () => origin.request(path, { announced: true }));
			await settle();
			let current = broadcasts[0];
			let unrelated: (typeof broadcasts)[number] | undefined;
			const samples: number[] = [];
			for (let round = 0; round < 6; round++) {
				const start = performance.now();
				for (let i = 0; i < 128; i++) {
					const nextPublication = origin.createBroadcast(touched === "watched" ? path : Path.from("other"));
					const next = nextPublication.broadcast;
					nextPublication.announce();
					if (touched === "watched") {
						current.close();
						current = next;
					} else {
						unrelated?.close();
						unrelated = next;
					}
					await settle();
					for (const request of requests)
						if (request.active.peek()?.closed !== current.closed)
							throw new Error("wrong publisher instance");
				}
				if (round > 0) samples.push(((performance.now() - start) * 1000) / 128);
			}
			samples.sort((a, b) => a - b);
			console.log(`${touched},${routes},${followers},${samples[2].toFixed(2)}`);
			for (const request of requests) request.close();
			current.close();
			unrelated?.close();
			for (const broadcast of broadcasts) broadcast.close();
			origin.close();
		}

// Cancelling a touched path must not scan all the origin's unrelated requests.
// Keep a second axis for the waiters sharing that path, which legitimately must exit.
console.log("unrelated,waiters,cancel_us");
for (const unrelated of [8, 128, 2048])
	for (const waiters of [1, 8, 32]) {
		const origin = new Producer();
		origin.requestsRoute(Path.from("pending"));
		const held = Array.from({ length: unrelated }, (_, i) => origin.request(Path.from(`pending/${i}`)));
		const wire = wireOf(origin.consume());
		const samples: number[] = [];
		for (let round = 0; round < 7; round++) {
			const holds = Array.from({ length: waiters }, () => new AbortController());
			const demands = holds.map((hold) =>
				wire.demand(Path.from("pending/touched"), undefined, hold.signal).catch(() => {}),
			);
			const start = performance.now();
			for (const hold of holds) hold.abort();
			await Promise.all(demands);
			if (round > 0) samples.push((performance.now() - start) * 1000);
		}
		samples.sort((a, b) => a - b);
		console.log(`${unrelated},${waiters},${samples[3].toFixed(2)}`);
		for (const request of held) request.close();
		origin.close();
	}

// An initial set landing settles only the pending requests beneath its prefix, never every
// request in the table, however many land in one task.
console.log("pending,landed,settle_us");
for (const pending of [128, 1024, 8192])
	for (const landed of [1, 16, 128]) {
		const samples: number[] = [];
		for (let round = 0; round < 5; round++) {
			const origin = new Producer();
			const wire = wireOf(origin);
			const detach = wire.attach(true);
			const initialSets = new Map<Path.Valid, () => void>();
			const stop = wire.interests((prefix, wanted) => {
				if (wanted) initialSets.set(prefix, wire.snapshot(prefix));
			});
			const held = Array.from({ length: pending }, (_, i) => origin.request(Path.from(`held/${i}`)));
			const touched = Array.from({ length: landed }, (_, i) => origin.request(Path.from(`touched/${i}`)));
			await settle();
			const start = performance.now();
			for (let i = 0; i < landed; i++) initialSets.get(Path.from(`touched/${i}`))?.();
			// The settlement is one microtask queued by the first landing, ahead of this one.
			await Promise.resolve();
			if (round > 0) samples.push((performance.now() - start) * 1000);
			if (touched.some((request) => request.closed.peek() === undefined)) throw new Error("absence unsettled");
			if (held.some((request) => request.closed.peek() !== undefined)) throw new Error("unsynced path settled");
			for (const request of [...held, ...touched]) request.close();
			for (const release of initialSets.values()) release();
			stop();
			detach();
			origin.close();
		}
		samples.sort((a, b) => a - b);
		console.log(`${pending},${landed},${samples[2].toFixed(2)}`);
	}

// Keep old instances on distinct same-epoch failovers, then cancel new demand.
// This exercises ownership checks inside one cleanup, independently of wire sharing.
console.log("failovers,extra_routes,cancel_us");
for (const failovers of [0, 1, 16, 128])
	for (const extraRoutes of [0, 128]) {
		const samples: number[] = [];
		for (let round = 0; round < 7; round++) {
			const origin = new Producer();
			const prefix = Path.from("room");
			const path = Path.from("room/video");
			const entries = [];
			const sources: Broadcast[] = [];
			for (let i = 0; i < failovers; i++) {
				const epoch = Epoch.mint();
				const primary = origin.requestsRoute(prefix, { epoch, cost: 0n });
				const ar = primary.requested();
				const br = origin.requestsRoute(prefix, { epoch, cost: 1n }).requested();
				const held = origin.request(path);
				const source = new Broadcast();
				sources.push(source);
				const answer = (await ar.next()).value;
				if (answer) hooks.acceptRequest(answer, source);
				await settle();
				entries.push({ primary, ar, br, held });
			}
			const epoch = Epoch.mint();
			origin.requestsRoute(prefix, { epoch });
			for (let i = 0; i < extraRoutes; i++) origin.requestsRoute(prefix, { epoch, cost: BigInt(i + 1) });
			const pending = [];
			for (const entry of entries) {
				entry.primary.close();
				const request = (await entry.br.next()).value;
				if (!request) throw new Error("missing failover request");
				pending.push(request);
			}
			const wire = wireOf(origin.consume());
			let active = 0;
			try {
				for (let i = 0; i < 128; i++) {
					const hold = new AbortController();
					const reason = new Error("cancelled");
					const wait = wire.demand(path, undefined, hold.signal).then(
						() => {
							throw new Error("cancelled demand resolved");
						},
						(error) => {
							if (error !== reason) throw error;
						},
					);
					const start = performance.now();
					hold.abort(reason);
					active += (performance.now() - start) * 1000;
					await wait;
				}
				// An ownership check must preserve every old instance, not just the winning one.
				for (let i = 0; i < entries.length; i++) {
					const source = new Broadcast();
					sources.push(source);
					hooks.acceptRequest(pending[i], source);
					await settle();
					if (!readsFrom(entries[i].held.active.peek(), source)) throw new Error("lost failover");
				}
				if (round > 1) samples.push(active / 128);
			} finally {
				for (const entry of entries) entry.held.close();
				origin.close();
				for (const source of sources) source.close();
				for (const entry of entries) {
					await entry.ar.return?.();
					await entry.br.return?.();
				}
			}
			await settle();
		}
		samples.sort((a, b) => a - b);
		console.log(`${failovers},${extraRoutes},${samples[2].toFixed(2)}`);
	}

// Batch teardown exposes repeated queue scans; draining checks that fast removal
// does not move that cost into ordered delivery. Local requests isolate draining
// from the shared wire-waiter settlement signal. Exclude request creation in both.
console.log("operation,paths,waiters,active_ms,settled_ms");
for (const operation of ["cancel-forward", "cancel-reverse", "drain-local"])
	for (const paths of [1024, 4096, 16384])
		for (const waiters of [1, 4]) {
			const active: number[] = [];
			const settled: number[] = [];
			for (let round = 0; round < 4; round++) {
				const origin = new Producer();
				const dynamic = origin.requestsRoute(Path.from("pending"));
				const requests = dynamic.requested();
				const wire = wireOf(origin.consume());
				const names = Array.from({ length: paths }, (_, i) => Path.from(`pending/${i}`));
				const held =
					operation === "drain-local"
						? names.flatMap((name) => Array.from({ length: waiters }, () => origin.request(name)))
						: [];
				const holds = Array.from(
					{ length: operation === "drain-local" ? 0 : paths * waiters },
					() => new AbortController(),
				);
				const demands = holds.map((hold, i) =>
					wire.demand(names[Math.floor(i / waiters)], undefined, hold.signal).catch(() => {}),
				);
				if (operation === "cancel-reverse") holds.reverse();
				try {
					const start = performance.now();
					if (operation === "drain-local") {
						for (const name of names) {
							const request = (await requests.next()).value;
							if (request?.path !== name) throw new Error("wrong request order");
							request.reject(new Error("refused"));
						}
					} else {
						for (const hold of holds) hold.abort();
					}
					const elapsed = performance.now() - start;
					await Promise.all(demands);
					if (round > 0) {
						active.push(elapsed);
						settled.push(performance.now() - start);
					}
				} finally {
					for (const request of held) request.close();
					origin.close();
					await requests.return?.();
				}
				await settle();
			}
			active.sort((a, b) => a - b);
			settled.sort((a, b) => a - b);
			console.log(`${operation},${paths},${waiters},${active[1].toFixed(2)},${settled[1].toFixed(2)}`);
		}

// One path with competing dynamic routes. Separate synchronous cancellation
// from promise settlement, and keep the surviving-waiter case distinct from
// releasing the final waiter, which legitimately withdraws the pending request.
console.log("survivor,routes,cancelled,active_us,settled_us");
for (const survivor of [false, true])
	for (const routes of [1, 16, 128])
		for (const cancelled of [1, 64, 1024]) {
			const active: number[] = [];
			const settled: number[] = [];
			for (let round = 0; round < 7; round++) {
				const origin = new Producer();
				const prefix = Path.from("competing");
				const path = Path.from("competing/touched");
				for (let i = 0; i < routes; i++) origin.requestsRoute(prefix, { cost: BigInt(i) });
				const wire = wireOf(origin.consume());
				const kept = survivor ? new AbortController() : undefined;
				const remaining = kept ? wire.demand(path, undefined, kept.signal).catch(() => {}) : undefined;
				const holds = Array.from({ length: cancelled }, () => new AbortController());
				const demands = holds.map((hold) => wire.demand(path, undefined, hold.signal).catch(() => {}));
				try {
					const start = performance.now();
					for (const hold of holds) hold.abort();
					const elapsed = performance.now() - start;
					await Promise.all(demands);
					if (round > 0) {
						active.push(elapsed * 1000);
						settled.push((performance.now() - start) * 1000);
					}
				} finally {
					kept?.abort();
					await remaining;
					origin.close();
				}
				await settle();
			}
			active.sort((a, b) => a - b);
			settled.sort((a, b) => a - b);
			console.log(`${survivor},${routes},${cancelled},${active[3].toFixed(2)},${settled[3].toFixed(2)}`);
		}

// Prefix updates withdraw the old set and replay the new set. Sweep both the
// namespaces per prefix and the concurrent requesting peers, including wire encoding.
console.log("namespace_update,draft,namespaces,subscribers,update_us");
for (const [version, alpn] of [
	[Version.DRAFT_18, ALPN.DRAFT_18],
	[Version.DRAFT_22, ALPN.DRAFT_22],
] as const)
	for (const namespaces of [1, 32, 128])
		for (const subscribers of [1, 8]) {
			const origin = new Producer();
			const sources = [];
			for (const prefix of ["a", "b"])
				for (let i = 0; i < namespaces; i++) {
					const sourcePublication = origin.createBroadcast(Path.from(`${prefix}/${i}`));
					const source = sourcePublication.broadcast;
					sourcePublication.announce();
					sources.push(source);
				}
			const peers = await Promise.all(
				Array.from({ length: subscribers }, async () => {
					const pair = createMockTransportPair(alpn);
					const pub = new IetfPublisher({
						quic: pair.server,
						session: new NativeSession(pair.server, version, false),
						publish: origin.consume(),
						requiresSolicitation: true,
					});
					const client = await Stream.open(pair.client, { version });
					const server = await Stream.accept(pair.server, version);
					if (!server) throw new Error("missing namespace stream");
					const running = pub.runSubscribeNamespace(
						new SubscribeNamespace({ requestId: 0n, namespace: Path.from("a") }),
						server,
					);
					if ((await client.reader.u53()) !== RequestOk.id) throw new Error("missing initial OK");
					await RequestOk.decode(client.reader, version);
					for (let i = 0; i < namespaces; i++) {
						if ((await client.reader.u53()) !== SubscribeNamespaceEntry.id)
							throw new Error("missing initial namespace");
						await SubscribeNamespaceEntry.decode(client.reader, version);
					}
					return { client, running };
				}),
			);
			const samples: number[] = [];
			try {
				for (let round = 0; round < 7; round++) {
					const start = performance.now();
					await Promise.all(
						peers.map(async ({ client }) => {
							await client.writer.write(
								Uint8Array.from([0x02, 0, 6, (round + 1) * 2, 1, 0x34, 1, 1, round % 2 ? 97 : 98]),
							);
							for (let i = 0; i < namespaces; i++) {
								if ((await client.reader.u53()) !== SubscribeNamespaceEntryDone.id)
									throw new Error("missing old withdrawal");
								await SubscribeNamespaceEntryDone.decode(client.reader, version);
							}
							if ((await client.reader.u53()) !== RequestOk.id) throw new Error("missing update OK");
							await RequestOk.decode(client.reader, version);
							for (let i = 0; i < namespaces; i++) {
								if ((await client.reader.u53()) !== SubscribeNamespaceEntry.id)
									throw new Error("missing replay namespace");
								await SubscribeNamespaceEntry.decode(client.reader, version);
							}
						}),
					);
					if (round > 1) samples.push((performance.now() - start) * 1000);
				}
				samples.sort((a, b) => a - b);
				console.log(
					`namespace_update,${version === Version.DRAFT_18 ? 18 : 21},${namespaces},${subscribers},${samples[2].toFixed(2)}`,
				);
			} finally {
				for (const peer of peers) peer.client.close();
				await Promise.all(peers.map((peer) => peer.running));
				for (const source of sources) source.close();
				origin.close();
			}
		}
