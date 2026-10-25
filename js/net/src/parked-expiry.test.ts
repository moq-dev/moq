import { expect, spyOn, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import { StreamCode } from "./error.ts";
import { Producer as Group } from "./group.ts";
import { randomHop } from "./hop.ts";
import { NativeSession } from "./ietf/adapter.ts";
import { Publisher as IetfPublisher } from "./ietf/publisher.ts";
import { Subscribe as IetfSubscribe } from "./ietf/subscribe.ts";
import { ALPN, Version as Ietf } from "./ietf/version.ts";
import { Publisher as LitePublisher } from "./lite/publisher.ts";
import { decodeSubscribeResponse, Subscribe as LiteSubscribe } from "./lite/subscribe.ts";
import { ALPN_05, ALPN_07_WIP, Version as Lite } from "./lite/version.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer as Origin } from "./origin.ts";
import * as Path from "./path.ts";
import { Stream, Writer } from "./stream.ts";
import { Timescale, Timestamp } from "./time.ts";

const protocols = [
	{ kind: "lite", version: Lite.DRAFT_05, alpn: ALPN_05 },
	{ kind: "lite", version: Lite.DRAFT_07, alpn: ALPN_07_WIP },
	{ kind: "ietf", version: Ietf.DRAFT_19, alpn: ALPN.DRAFT_19 },
	{ kind: "ietf", version: Ietf.DRAFT_22, alpn: ALPN.DRAFT_22 },
] as const;

for (const protocol of protocols) {
	test.each(["expires", "is evicted"] as const)(
		`${protocol.alpn}: a parked group open ends when its group %s`,
		async (mode) => {
			const clock = new Manual();
			const installed = clock.install();
			const pair = createMockTransportPair(protocol.alpn);
			const origin = new Origin();
			const broadcast = origin.createBroadcast(Path.from("parked"));
			broadcast.announce();
			const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
			const write = (sequence: number) => {
				const group = new Group(sequence);
				group.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.fromMillis(0) });
				group.close();
				track.writeGroup(group);
			};

			write(0);

			const parked = Promise.withResolvers<void>();
			const slot = Promise.withResolvers<void>();
			let writes = 0;
			let resets = 0;
			const late = new WritableStream<Uint8Array>({
				write: () => {
					writes++;
				},
				abort: () => {
					resets++;
				},
			});
			const opening = spyOn(pair.server, "createUnidirectionalStream").mockImplementationOnce(async () => {
				parked.resolve();
				await slot.promise;
				return late;
			});
			const tryOpen = Writer.tryOpen;
			let ended = false;
			const trying = spyOn(Writer, "tryOpen").mockImplementationOnce(async (...args) => {
				try {
					// Isolate eviction from the independent ten-second open fallback.
					const [quic, options] = args;
					return await tryOpen(quic, { ...options, timeout: mode === "is evicted" ? null : options.timeout });
				} finally {
					ended = true;
				}
			});
			const client = await Stream.open(pair.client, { version: protocol.version });
			const server = await Stream.accept(pair.server, protocol.version);
			if (!server) throw new Error("missing subscribe stream");
			const budget = mode === "expires" ? 100 : 60_000;
			let running: Promise<void>;
			let close = () => {};
			if (protocol.kind === "lite") {
				const publisher = new LitePublisher(pair.server, protocol.version, randomHop(), origin.consume());
				close = () => publisher.close();
				running = publisher.runSubscribe(
					new LiteSubscribe({
						id: 0n,
						broadcast: Path.from("parked"),
						track: "video",
						priority: 0,
						maxDelay: budget,
					}),
					server,
				);
			} else {
				const publisher = new IetfPublisher({
					quic: pair.server,
					session: new NativeSession(pair.server, protocol.version, true),
					publish: origin.consume(),
					requiresSolicitation: false,
				});
				running = publisher.runSubscribe(
					new IetfSubscribe({
						requestId: 0n,
						trackNamespace: Path.from("parked"),
						trackName: "video",
						subscriberPriority: 0,
						maxAge: budget,
					}),
					server,
				);
			}
			try {
				await parked.promise;
				expect(ended).toBe(false);
				write(1);
				if (mode === "expires") {
					await clock.advance(Nano.fromMilli(Milli(budget - 1)));
					expect(ended).toBe(false);
					await clock.advance(Nano.fromMilli(Milli(1)));
				} else await clock.advance(Nano.fromMilli(Milli(33_750)));
				expect(ended).toBe(true);
				if (protocol.kind === "lite") {
					for (;;) {
						const response = await decodeSubscribeResponse(client.reader, protocol.version);
						if (!("drop" in response) || response.drop.start !== 0) continue;
						const cause = mode === "expires" ? StreamCode.Old : StreamCode.TooFarBehind;
						expect(response.drop.error).toBe(protocol.version === Lite.DRAFT_07 ? cause : 0);
						break;
					}
				}
				// The transport cannot cancel createUnidirectionalStream. A late slot is reset,
				// never used to transmit the obsolete group's header or frames.
				slot.resolve();
				await clock.advance(Nano.zero);
				expect(writes).toBe(0);
				expect(resets).toBe(1);
			} finally {
				slot.resolve();
				close();
				client.close();
				broadcast.close();
				origin.close();
				await running;
				opening.mockRestore();
				trying.mockRestore();
				installed.close();
			}
		},
	);
}

for (const protocol of protocols) {
	test(`${protocol.alpn}: a blocked group write ends when eviction takes what it has yet to read`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		const pair = createMockTransportPair(protocol.alpn);
		const origin = new Origin();
		const broadcast = origin.createBroadcast(Path.from("parked"));
		broadcast.announce();
		const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
		const write = (sequence: number, frames: number) => {
			const group = new Group(sequence);
			for (let frame = 0; frame < frames; frame++) {
				group.writeFrame({ payload: new Uint8Array([frame]), timestamp: Timestamp.fromMillis(0) });
			}
			group.close();
			track.writeGroup(group);
		};

		// Two frames, so the second is still unread while the stream's first write waits on credit.
		write(0, 2);

		const blocked = Promise.withResolvers<void>();
		const credit = Promise.withResolvers<void>();
		let writes = 0;
		let resets = 0;
		const stalled = new WritableStream<Uint8Array>({
			write: async () => {
				writes++;
				blocked.resolve();
				await credit.promise;
			},
			abort: () => {
				resets++;
			},
		});
		const opening = spyOn(pair.server, "createUnidirectionalStream").mockImplementationOnce(async () => stalled);
		const client = await Stream.open(pair.client, { version: protocol.version });
		const server = await Stream.accept(pair.server, protocol.version);
		if (!server) throw new Error("missing subscribe stream");
		// A budget past the eviction, so only the eviction can give the group up.
		const budget = 60_000;
		let running: Promise<void>;
		let close = () => {};
		if (protocol.kind === "lite") {
			const publisher = new LitePublisher(pair.server, protocol.version, randomHop(), origin.consume());
			close = () => publisher.close();
			running = publisher.runSubscribe(
				new LiteSubscribe({
					id: 0n,
					broadcast: Path.from("parked"),
					track: "video",
					priority: 0,
					maxDelay: budget,
				}),
				server,
			);
		} else {
			const publisher = new IetfPublisher({
				quic: pair.server,
				session: new NativeSession(pair.server, protocol.version, true),
				publish: origin.consume(),
				requiresSolicitation: false,
			});
			running = publisher.runSubscribe(
				new IetfSubscribe({
					requestId: 0n,
					trackNamespace: Path.from("parked"),
					trackName: "video",
					subscriberPriority: 0,
					maxAge: budget,
				}),
				server,
			);
		}
		try {
			await blocked.promise;
			write(1, 1);
			await clock.advance(Nano.fromMilli(Milli(33_750)));
			// The write already handed to the stream finishes, and the stream is reset after it.
			credit.resolve();
			await clock.advance(Nano.zero);
			expect(writes).toBe(1);
			expect(resets).toBe(1);
		} finally {
			credit.resolve();
			close();
			client.close();
			broadcast.close();
			origin.close();
			await running;
			opening.mockRestore();
			installed.close();
		}
	});
}
