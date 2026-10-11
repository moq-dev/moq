/** Compare announced request following against the previous follow-and-request composition. */
import type * as Announce from "../src/announced";
import { Producer } from "../src/origin";
import * as Path from "../src/path";

const legacy = process.argv.includes("--legacy");
const settle = () => new Promise<void>((resolve) => setImmediate(resolve));
console.log("mode,touched,routes,followers,update_us");
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
			const readers = Array.from({ length: followers }, () => {
				let request = origin.request(path, { announced: !legacy });
				const stream = legacy
					? (origin as unknown as { follow(path: Path.Valid): Announce.Consumer }).follow(path)
					: undefined;
				const done = (async () => {
					if (!stream) return;
					for await (const event of stream) {
						if (event.kind !== "start" && event.kind !== "restart") continue;
						const previous = request;
						request = origin.request(path);
						previous.close();
					}
				})();
				return {
					active: () => request.active.peek(),
					close: async () => {
						stream?.close();
						await done;
						request.close();
					},
				};
			});
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
					for (const reader of readers)
						if (reader.active()?.closed !== current.closed) throw new Error("wrong publisher instance");
				}
				if (round > 0) samples.push(((performance.now() - start) * 1000) / 128);
			}
			samples.sort((a, b) => a - b);
			console.log(`${legacy ? "legacy" : "folded"},${touched},${routes},${followers},${samples[2].toFixed(2)}`);
			await Promise.all(readers.map((reader) => reader.close()));
			current.close();
			unrelated?.close();
			for (const broadcast of broadcasts) broadcast.close();
			origin.close();
		}
