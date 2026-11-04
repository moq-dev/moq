import { expect, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import { SessionCode, SessionError, StreamCode, StreamError } from "./error.ts";
import { HopSchema } from "./hop.ts";
import { NativeSession } from "./ietf/session.ts";
import { Subscriber as IetfSubscriber } from "./ietf/subscriber.ts";
import { Version as Ietf } from "./ietf/version.ts";
import { Subscriber as LiteSubscriber } from "./lite/subscriber.ts";
import { Version as Lite } from "./lite/version.ts";
import * as Path from "./path.ts";

// A transport whose creates wait for stream credit until `release`, or open at once.
function limited(mode: "queue" | "open") {
	const slot = Promise.withResolvers<void>();
	const created = Promise.withResolvers<void>();
	const aborted = Promise.withResolvers<unknown>();
	const written: Uint8Array[] = [];
	const options: unknown[] = [];
	const stream = () => ({
		readable: new ReadableStream<Uint8Array>(),
		writable: new WritableStream<Uint8Array>({
			abort: aborted.resolve,
			write: (bytes) => {
				written.push(new Uint8Array(bytes));
			},
		}),
	});
	const quic = {
		closed: new Promise(() => {}),
		createBidirectionalStream: (opts: unknown) => {
			options.push(opts);
			created.resolve();
			return mode === "open" ? Promise.resolve(stream()) : slot.promise.then(stream);
		},
	} as unknown as WebTransport;
	return {
		quic,
		created: created.promise,
		aborted: aborted.promise,
		written,
		options,
		release: () => slot.resolve(),
	};
}

// Waiting for stream credit is waiting its turn, so it never fails a request: the deadline
// runs only once the request's streams are open, for an answer that never comes.
for (const protocol of ["lite", "ietf"] as const) {
	for (const mode of ["queue", "open"] as const) {
		test(`${protocol}: ${mode} has one fatal request deadline from its open`, async () => {
			const clock = new Manual();
			const installed = clock.install();
			const transport = limited(mode);
			const subscriber =
				protocol === "lite"
					? new LiteSubscriber(transport.quic, Lite.DRAFT_07, HopSchema.parse(1n))
					: new IetfSubscriber({ session: new NativeSession(transport.quic, Ietf.DRAFT_18, true) });
			const broadcast = subscriber.consume(Path.from("room"));
			const track = broadcast.track("video").subscribe();
			try {
				await transport.created;
				await clock.advance(Nano.zero);
				if (mode === "queue") {
					await clock.advance(Nano.fromMilli(Milli(30_000)));
					expect(track.closed.peek()).toBeUndefined();
					expect(transport.written).toEqual([]);
					transport.release();
					await clock.advance(Nano.zero);
					expect(transport.written).not.toEqual([]);
				}
				await clock.advance(Nano.fromMilli(Milli(9_999)));
				expect(track.closed.peek()).toBeUndefined();
				await clock.advance(Nano.fromMilli(Milli(1)));
				const failure = await track.closed;
				expect(failure).toBeInstanceOf(StreamError);
				expect((failure as StreamError).code).toBe(StreamCode.ControlTimeout);
				await transport.aborted;
				// A lite subscription opens its SUBSCRIBE alongside its TRACK, under the one deadline.
				const opened = protocol === "lite" ? 2 : 1;
				expect(transport.options).toEqual(Array.from({ length: opened }, () => ({ waitUntilAvailable: true })));
				await clock.advance(Nano.fromMilli(Milli(30_000)));
				expect(transport.options).toHaveLength(opened);
			} finally {
				track.close();
				broadcast.close();
				if (subscriber instanceof LiteSubscriber) subscriber.close();
				installed.close();
			}
		});
	}
}

for (const operation of ["info", "fetch"] as const) {
	for (const mode of ["queue", "open"] as const) {
		test(`lite standalone ${operation}: ${mode} has a fatal deadline from its open`, async () => {
			const clock = new Manual();
			const installed = clock.install();
			const transport = limited(mode);
			const subscriber = new LiteSubscriber(transport.quic, Lite.DRAFT_07, HopSchema.parse(1n));
			let failure: unknown;
			const request = (
				operation === "info"
					? subscriber.resolveTrackInfo(Path.from("room"), "video")
					: subscriber.fetchGroup(Path.from("room"), "video", 0n)
			).catch((err: unknown) => {
				failure = err;
			});
			try {
				await transport.created;
				await clock.advance(Nano.zero);
				if (mode === "queue") {
					await clock.advance(Nano.fromMilli(Milli(30_000)));
					expect(failure).toBeUndefined();
					transport.release();
					await clock.advance(Nano.zero);
				}
				await clock.advance(Nano.fromMilli(Milli(10_000)));
				await request;
				expect((failure as StreamError).code).toBe(StreamCode.ControlTimeout);
				// A fetch opens its FETCH stream alongside the TRACK stream, under the same deadline.
				expect(transport.options).toHaveLength(operation === "info" ? 1 : 2);
				// The FETCH stream opens at its group's send order.
				expect(transport.options[0]).toEqual({ waitUntilAvailable: true });
				for (const open of transport.options) expect(open).toMatchObject({ waitUntilAvailable: true });
			} finally {
				subscriber.close();
				installed.close();
			}
		});
	}
}

for (const protocol of ["lite", "ietf"] as const) {
	for (const mode of ["queue", "open"] as const) {
		test(`${protocol} discovery: ${mode} waits for credit and has no deadline`, async () => {
			const clock = new Manual();
			const installed = clock.install();
			const transport = limited(mode);
			const subscriber =
				protocol === "lite"
					? new LiteSubscriber(transport.quic, Lite.DRAFT_07, HopSchema.parse(1n))
					: new IetfSubscriber({ session: new NativeSession(transport.quic, Ietf.DRAFT_18, true) });
			const announced = subscriber.announced();
			try {
				await transport.created;
				await clock.advance(Nano.zero);
				await clock.advance(Nano.fromMilli(Milli(30_000)));
				expect(announced.closed.peek()).toBeUndefined();
				if (mode === "queue") {
					expect(transport.written).toEqual([]);
					transport.release();
					await clock.advance(Nano.zero);
					expect(transport.written).not.toEqual([]);
				}
				expect(transport.options).toEqual([{ waitUntilAvailable: true }]);
			} finally {
				announced.close();
				if (subscriber instanceof LiteSubscriber) subscriber.close();
				installed.close();
			}
		});
	}
}

for (const protocol of ["lite", "ietf"] as const) {
	test(`${protocol}: a create the closed session fails reports the session`, async () => {
		const quic = {
			closed: Promise.resolve({ closeCode: SessionCode.Unauthorized, reason: "bye" }),
			createBidirectionalStream: () =>
				Promise.reject(
					Object.assign(new Error("session closed"), { source: "session", streamErrorCode: null }),
				),
		} as unknown as WebTransport;
		const subscriber =
			protocol === "lite"
				? new LiteSubscriber(quic, Lite.DRAFT_07, HopSchema.parse(1n))
				: new IetfSubscriber({ quic, session: new NativeSession(quic, Ietf.DRAFT_18, true) });
		const broadcast = subscriber.consume(Path.from("room"));
		const track = broadcast.track("video").subscribe();
		try {
			const failure = await track.closed;
			expect(failure).toBeInstanceOf(SessionError);
			expect((failure as SessionError).code).toBe(SessionCode.Unauthorized);
		} finally {
			track.close();
			broadcast.close();
			if (subscriber instanceof LiteSubscriber) subscriber.close();
		}
	});
}
