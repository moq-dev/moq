/** Sweep announced request followers against routes for one restart of a watched or unrelated path. */
import { Producer } from "../src/origin";
import * as Path from "../src/path";
import { wireOf } from "../src/wire";

const settle = () => new Promise<void>((resolve) => setImmediate(resolve));
console.log("touched,routes,followers,update_us");
for (const touched of ["watched", "unrelated"])
	for (const routes of [8, 32, 128])
		for (const followers of [1, 8, 32]) {
			const origin = new Producer();
			const path = Path.from("room/0");
			const broadcasts = Array.from({ length: routes }, (_, i) => {
				const broadcast = origin.createBroadcast(Path.from(`room/${i}`));
				broadcast.announce();
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
					const next = origin.createBroadcast(touched === "watched" ? path : Path.from("other"));
					next.announce();
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
		origin.dynamic(Path.from("pending"));
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
				const dynamic = origin.dynamic(Path.from("pending"));
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
