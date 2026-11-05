import { expect, spyOn, test } from "bun:test";
import { SessionCode, StreamCode, StreamError } from "../error.ts";
import { HopSchema } from "../hop.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream, Writer } from "../stream.ts";
import { Slots } from "../util/limits.ts";
import { Connection } from "./connection.ts";
import { PublishNamespace } from "./publish_namespace.ts";
import { RequestOk } from "./request.ts";
import { NativeSession } from "./session.ts";
import { Subscribe, SubscribeOk } from "./subscribe.ts";
import { SubscribeNamespaceEntry, SubscribeNamespaceEntryDone } from "./subscribe_namespace.ts";
import { Subscriber } from "./subscriber.ts";
import { ALPN, Version } from "./version.ts";

const versions = [
	[Version.DRAFT_18, ALPN.DRAFT_18],
	[Version.DRAFT_22, ALPN.DRAFT_22],
] as const;

// Both connection drivers run, while the client can inject a malformed peer stream.
for (const [version, alpn] of versions) {
	test.each(["update", "unsolicited"] as const)(
		`${alpn}: %s closes the peer session with its violation`,
		async (kind) => {
			const pair = createMockTransportPair(alpn);
			const clientControl = await Stream.open(pair.client, { version });
			const serverControl = await Stream.accept(pair.server, version);
			if (!serverControl) throw new Error("missing control stream");
			const server = new Connection({
				url: new URL("https://example.com"),
				quic: pair.server,
				control: serverControl,
				version,
				client: false,
				solicit: true,
			});
			const client = new Connection({
				url: new URL("https://example.com"),
				quic: pair.client,
				control: clientControl,
				version,
				client: true,
				solicit: true,
			});
			const closing = spyOn(pair.server, "close");
			const logged = spyOn(console, "error").mockImplementation(() => {});
			try {
				const stream = await Stream.open(pair.client, { version });
				if (kind === "update") await stream.writer.u62(0x02n);
				else {
					await stream.writer.u53(PublishNamespace.id);
					await new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("unasked") }).encode(
						stream.writer,
						version,
					);
				}
				// The handler either ends the stream or the session before this read settles.
				await stream.reader.u53().catch(() => undefined);
				expect(closing.mock.calls[0]?.[0]?.closeCode).toBe(SessionCode.ProtocolViolation);
				expect((await pair.client.closed).closeCode).toBe(SessionCode.ProtocolViolation);
			} finally {
				client.abort();
				server.abort();
				closing.mockRestore();
				logged.mockRestore();
			}
		},
	);

	test(`${alpn}: a shared alias for different tracks closes with a violation`, async () => {
		const pair = createMockTransportPair(alpn);
		const subscriber = new Subscriber({
			session: new NativeSession(pair.server, version, true),
			quic: pair.server,
		});
		const broadcast = subscriber.consume(Path.from("room"));
		const tracks = [];
		try {
			for (const name of ["video", "audio"]) {
				const track = broadcast.track(name).subscribe();
				tracks.push(track);
				const peer = await Stream.accept(pair.client, version);
				if (!peer) throw new Error("missing subscription");
				expect(await peer.reader.u53()).toBe(Subscribe.id);
				await Subscribe.decode(peer.reader, version);
				await peer.writer.u53(SubscribeOk.id);
				await new SubscribeOk({ trackAlias: 7n }).encode(peer.writer, version);
				await track.info();
			}
			expect((await pair.client.closed).closeCode).toBe(SessionCode.ProtocolViolation);
		} finally {
			for (const track of tracks) track.close();
			broadcast.close();
			pair.client.close();
			pair.server.close();
		}
	});

	test(`${alpn}: reflected namespaces retain a slot through repeats until DONE`, async () => {
		const pair = createMockTransportPair(alpn);
		const chunks: Uint8Array[] = [];
		const writer = new Writer(
			new WritableStream({
				write: (chunk: Uint8Array) => {
					chunks.push(chunk.slice());
				},
			}),
			version,
		);
		await writer.u53(RequestOk.id);
		await new RequestOk({}).encode(writer, version);
		const self = HopSchema.parse(5n),
			peer = HopSchema.parse(9n);
		const entry = async (name: string) => {
			await writer.u53(SubscribeNamespaceEntry.id);
			await new SubscribeNamespaceEntry({
				suffix: Path.from(name),
				params: { cluster: { hops: [self, peer], cost: 0n } },
			}).encode(writer, version);
		};
		await entry("a");
		await writer.u53(SubscribeNamespaceEntryDone.id);
		await new SubscribeNamespaceEntryDone({ suffix: Path.from("a") }).encode(writer, version);
		await entry("b");
		await entry("b");
		await entry("c");
		writer.close();
		await writer.closed;
		const readable = new ReadableStream<Uint8Array>({
			start(c) {
				for (const chunk of chunks) c.enqueue(chunk);
				c.close();
			},
		});
		const session = new NativeSession(pair.server, version, true);
		spyOn(session, "openBi").mockImplementation(
			async () => new Stream({ version, readable, writable: new WritableStream() }),
		);
		const subscriber = new Subscriber({
			session,
			quic: pair.server,
			cluster: { self, peer },
			limits: { announces: 1 },
		});
		const acquiring = spyOn(Slots.prototype, "tryAcquire");
		try {
			const announced = subscriber.announced();
			await announced.closed;
			expect(acquiring).toHaveBeenCalledTimes(3); // a, b, c; neither DONE nor the repeat takes another slot.
			expect((await pair.client.closed).closeCode).toBe(SessionCode.TooManyRequests);
		} finally {
			acquiring.mockRestore();
			pair.client.close();
			pair.server.close();
		}
	});

	test.each(["done", "end"] as const)(
		`${alpn}: %s releases a slot after route reflection and reattachment`,
		async (ending) => {
			const pair = createMockTransportPair(alpn);
			const self = HopSchema.parse(5n),
				peer = HopSchema.parse(9n);
			const subscriber = new Subscriber({
				session: new NativeSession(pair.server, version, true),
				quic: pair.server,
				cluster: { self, peer },
				limits: { announces: 1 },
			});
			const announced = subscriber.announced();
			const stream = await Stream.accept(pair.client, version);
			if (!stream) throw new Error("missing namespace stream");
			const answer = async (stream: Stream) => {
				await stream.writer.u53(RequestOk.id);
				await new RequestOk({}).encode(stream.writer, version);
			};
			const entry = async (stream: Stream, name: string, reflected = false) => {
				await stream.writer.u53(SubscribeNamespaceEntry.id);
				await new SubscribeNamespaceEntry({
					suffix: Path.from(name),
					params: { cluster: { hops: reflected ? [self, peer] : [peer], cost: 0n } },
				}).encode(stream.writer, version);
			};
			try {
				await answer(stream);
				await entry(stream, "a");
				expect((await announced.next())?.kind).toBe("start");
				await entry(stream, "a", true);
				expect((await announced.next())?.kind).toBe("end");
				await entry(stream, "a");
				expect((await announced.next())?.kind).toBe("start");
				if (ending === "done") {
					await stream.writer.u53(SubscribeNamespaceEntryDone.id);
					await new SubscribeNamespaceEntryDone({ suffix: Path.from("a") }).encode(stream.writer, version);
					expect((await announced.next())?.kind).toBe("end");
					await entry(stream, "b");
					expect((await announced.next())?.prefix).toBe(Path.from("b"));
				} else {
					stream.abort(new StreamError(StreamCode.Cancel));
					await announced.closed;
					const next = subscriber.announced();
					const replacement = await Stream.accept(pair.client, version);
					if (!replacement) throw new Error("missing replacement namespace stream");
					await answer(replacement);
					await entry(replacement, "b");
					expect((await next.next())?.prefix).toBe(Path.from("b"));
					next.close();
				}
			} finally {
				announced.close();
				pair.client.close();
				pair.server.close();
			}
		},
	);

	test(`${alpn}: inline and unsolicited advertisements share the same budget`, async () => {
		const pair = createMockTransportPair(alpn);
		const subscriber = new Subscriber({
			session: new NativeSession(pair.server, version, true),
			quic: pair.server,
			limits: { announces: 1 },
		});
		const announced = subscriber.announced();
		try {
			const inline = await Stream.accept(pair.client, version);
			if (!inline) throw new Error("missing namespace stream");
			await inline.writer.u53(RequestOk.id);
			await new RequestOk({}).encode(inline.writer, version);
			await inline.writer.u53(SubscribeNamespaceEntry.id);
			await new SubscribeNamespaceEntry({ suffix: Path.from("a") }).encode(inline.writer, version);
			expect((await announced.next())?.prefix).toBe(Path.from("a"));
			const peer = await Stream.open(pair.client, { version });
			const stream = await Stream.accept(pair.server, version);
			if (!stream) throw new Error("missing unsolicited namespace stream");
			// Even the same path has two independently held wire advertisements.
			await subscriber
				.runPublishNamespace(new PublishNamespace({ requestId: 1n, trackNamespace: Path.from("a") }), stream)
				.catch(() => {});
			expect((await pair.client.closed).closeCode).toBe(SessionCode.TooManyRequests);
			peer.close();
		} finally {
			announced.close();
			pair.client.close();
			pair.server.close();
		}
	});
}
