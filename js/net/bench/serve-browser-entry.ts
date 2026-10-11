/** Measure publisher fairness with memory-resident groups and an immediately writable transport. */

import { randomHop } from "../src/hop.ts";
import { NativeSession } from "../src/ietf/adapter.ts";
import { Publisher as IetfPublisher } from "../src/ietf/publisher.ts";
import { Subscribe as IetfSubscribe } from "../src/ietf/subscribe.ts";
import { ALPN, Version as IetfVersion } from "../src/ietf/version.ts";
import { Publisher as LitePublisher } from "../src/lite/publisher.ts";
import { Subscribe as LiteSubscribe } from "../src/lite/subscribe.ts";
import { ALPN_05, Version as LiteVersion } from "../src/lite/version.ts";
import { createMockTransportPair } from "../src/mock.ts";
import { Producer as Origin } from "../src/origin.ts";
import * as Path from "../src/path.ts";
import { Stream } from "../src/stream.ts";
import { Milli, Timescale, Timestamp } from "../src/time.ts";

async function measure(protocol: string, groups: number, viewers: number) {
	const origin = new Origin();
	const broadcast = origin.createBroadcast(Path.from("bench"));
	broadcast.announce();
	const track = broadcast.createTrack("data", { maxAge: Milli(60_000), timescale: Timescale.MICRO });
	const pair = createMockTransportPair(protocol === "lite" ? ALPN_05 : ALPN.DRAFT_22);
	const version = protocol === "lite" ? LiteVersion.DRAFT_05 : IetfVersion.DRAFT_22;
	let written = 0,
		closed = 0;
	let finish!: () => void;
	let fail!: (err: unknown) => void;
	const done = new Promise<void>((resolve, reject) => {
		finish = resolve;
		fail = reject;
	});
	pair.server.createUnidirectionalStream = async () =>
		new WritableStream<Uint8Array>({
			write(chunk) {
				written += chunk.byteLength;
			},
			close() {
				if (++closed === groups * viewers) finish();
			},
		});
	const publisher =
		protocol === "lite"
			? new LitePublisher(pair.server, LiteVersion.DRAFT_05, randomHop(), origin.consume())
			: new IetfPublisher({
					quic: pair.server,
					session: new NativeSession(pair.server, IetfVersion.DRAFT_22, true),
					publish: origin.consume(),
					requiresSolicitation: false,
				});
	const streams: Stream[] = [];
	for (let viewer = 0; viewer < viewers; viewer++) {
		const stream = new Stream({
			version,
			readable: new ReadableStream(),
			writable: new WritableStream<Uint8Array>(),
		});
		streams.push(stream);
		const running =
			protocol === "lite"
				? (publisher as LitePublisher).runSubscribe(
						new LiteSubscribe({
							id: BigInt(viewer),
							broadcast: Path.from("bench"),
							track: "data",
							priority: 0,
							startGroup: 0,
							maxDelay: Milli(60_000),
						}),
						stream,
					)
				: (publisher as IetfPublisher).runSubscribe(
						new IetfSubscribe({
							requestId: BigInt(viewer),
							trackNamespace: Path.from("bench"),
							trackName: "data",
							subscriberPriority: 0,
							filter: { kind: "absolute", startGroup: 0n, startObject: 0n },
						}),
						stream,
					);
		void running.catch(fail);
	}
	let ticks = 0,
		maxGap = 0;
	let last = performance.now();
	const timer = setInterval(() => {
		const now = performance.now();
		maxGap = Math.max(maxGap, now - last);
		last = now;
		ticks++;
	}, 0);
	const start = performance.now();
	for (let sequence = 0; sequence < groups; sequence++) {
		const group = track.appendGroup();
		for (let frame = 0; frame < 128; frame++)
			group.writeFrame({
				payload: new Uint8Array(32),
				timestamp: Timestamp.fromMicros(sequence * 2500 + Math.floor((frame * 2500) / 128)),
			});
		group.close();
	}
	let deadline: ReturnType<typeof setTimeout> | undefined;
	try {
		await Promise.race([
			done,
			new Promise<never>((_, reject) => {
				deadline = setTimeout(
					() => reject(new Error(`only ${closed}/${groups * viewers} streams, ${written} bytes`)),
					10_000,
				);
			}),
		]);
		const now = performance.now();
		const ms = now - start;
		maxGap = Math.max(maxGap, now - last);
		return { protocol, groups, viewers, frames: groups * viewers * 128, ms, ticks, maxGap, written };
	} finally {
		clearTimeout(deadline);
		clearInterval(timer);
		publisher.close();
		for (const stream of streams) stream.close();
		track.close();
		broadcast.close();
		origin.close();
		pair.client.close();
		pair.server.close();
	}
}

(globalThis as unknown as { measure: typeof measure }).measure = measure;
