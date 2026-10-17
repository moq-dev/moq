// Sweep protected frame size x writes in flight through the serial pump, then open every frame.
// Bun's WebCrypto stands in for the browser's, so compare runs on the same runtime.
import { Milli } from "@moq/clock";
import { Broadcast, Time } from "@moq/net";
import { Credential } from "../src/index.ts";

const GROUP_FRAMES = 64;
const BUDGET = 32 * 1024 * 1024;
const credential = new Credential({
	context: new TextEncoder().encode("bench"),
	kid: 1,
	secret: crypto.getRandomValues(new Uint8Array(32)),
});

const median = (values: number[]) => values.sort((a, b) => a - b)[Math.floor(values.length / 2)];

console.log("bytes,depth,frames,seal_us,open_us");
for (const bytes of [160, 1_160, 16_384, 262_144]) {
	const frames = Math.min(1024, Math.max(64, Math.floor(BUDGET / bytes)));
	const payload = new Uint8Array(bytes);
	for (const depth of [1, 8, 64]) {
		const seal: number[] = [];
		const open: number[] = [];
		// The first round warms the JIT and key imports and is discarded.
		for (let round = 0; round < 6; round++) {
			const generation = credential.produce();
			const net = new Broadcast.Producer().createTrack(await generation.name("video"), {
				timescale: Time.Timescale.MILLI,
			});
			const consumer = credential.consume(generation.epoch).consume(net.subscribe({ maxDelay: Milli(60_000) }));
			const producer = await generation.produce(net);

			const sealStart = performance.now();
			const pending: Promise<void>[] = [];
			let group = producer.appendGroup();
			for (let frame = 0; frame < frames; frame++) {
				if (frame > 0 && frame % GROUP_FRAMES === 0) {
					group.close();
					group = producer.appendGroup();
				}
				pending.push(group.writeFrame(Time.Timestamp.fromMillis(frame), payload));
				if (pending.length >= depth) await pending.shift();
			}
			await Promise.all(pending);
			group.close();
			const sealed = performance.now();

			let read = 0;
			while (read < frames) {
				const received = await consumer.recvGroup();
				if (!received) throw new Error("track ended before every frame was read");
				while (await received.readFrame()) read++;
			}
			const opened = performance.now();
			producer.close();
			consumer.close();

			if (round === 0) continue;
			seal.push(((sealed - sealStart) * 1000) / frames);
			open.push(((opened - sealed) * 1000) / frames);
		}
		console.log(`${bytes},${depth},${frames},${median(seal).toFixed(2)},${median(open).toFixed(2)}`);
	}
}
