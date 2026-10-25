/** Sweep retained groups, unrelated tracks, and joined readers for a refresh crossing one lite hop. */
import { Manual, Nano } from "@moq/clock";
import { accept } from "../src/connection/accept.ts";
import { connect } from "../src/connection/connect.ts";
import { ALPN_07_WIP } from "../src/lite/version.ts";
import { createMockTransportPair } from "../src/mock.ts";
import { Producer } from "../src/origin.ts";
import * as Path from "../src/path.ts";

console.debug = () => {};
console.log("retained,tracks,joined,refresh_us");
for (const retained of [1, 64, 1024])
	for (const tracks of [1, 8, 32])
		for (const joined of [1, 8, 32]) {
			const clock = new Manual();
			const installed = clock.install();
			const source = new Producer();
			const destination = new Producer();
			const broadcast = source.createBroadcast(Path.from("room"));
			broadcast.announce();
			const producers = Array.from({ length: tracks }, (_, i) =>
				broadcast.createTrack(`data${i}`, { refresh: 1 }),
			);
			const pair = createMockTransportPair(ALPN_07_WIP);
			const url = new URL("https://localhost/refresh");
			const peers = await Promise.all([
				connect({ url, transport: pair.client, consume: destination }),
				accept({ url, transport: pair.server, publish: source.consume() }),
			]);
			try {
				const announcements = destination.announced();
				while ((await announcements.next())?.kind !== "start") {}
				const request = destination.request(Path.from("room"));
				while (!request.active.peek()) await request.active.changed();
				const remote = request.active.peek();
				if (!remote) throw new Error("broadcast missing");
				const consumers = producers.map((_, i) => remote.track(`data${i}`));
				const baseline = consumers.map((track) => track.subscribe());
				for (let i = 0; i < tracks; i++) {
					const group = producers[i].appendGroup();
					group.writeFrame({ payload: new Uint8Array(3) });
					group.finish();
					const received = await baseline[i].recvGroup();
					if (!received) throw new Error("baseline group missing");
					await received.readFrame();
					received.close();
				}
				// Keep this track's historical groups in its cache. Only the touched
				// track grows, so scanning that cache appears independently of fanout.
				for (let old = 1; old < retained; old++) {
					const group = producers[0].appendGroup();
					group.writeFrame({ payload: new Uint8Array(3) });
					group.finish();
					const received = await baseline[0].recvGroup();
					if (!received) throw new Error("historical group missing");
					await received.readFrame();
					received.close();
				}

				const samples: number[] = [];
				for (let round = 0; round < 9; round++) {
					const group = producers[0].appendGroup();
					group.writeFrame({ payload: new Uint8Array(3) });
					group.writeFrame({ payload: new Uint8Array(4) });
					group.finish();
					const received = await baseline[0].recvGroup();
					if (!received) throw new Error("measured group missing");
					await received.readFrame();
					await received.readFrame();
					received.close();
					const start = performance.now();
					const joins = Array.from({ length: joined }, () => consumers[0].subscribe());
					const wanted = producers[0].demand().wanted;
					while (wanted.peek() !== group.sequence + 1) await wanted.changed();
					if (round > 0) samples.push((performance.now() - start) * 1000);
					for (const join of joins) join.close();
				}
				samples.sort((a, b) => a - b);
				console.log(`${retained},${tracks},${joined},${samples[4].toFixed(2)}`);
				for (const sub of baseline) sub.close();
				request.close();
			} finally {
				destination.close();
				source.close();
				for (const peer of peers) peer.abort();
				await clock.advance(Nano(0));
				installed.close();
			}
		}
