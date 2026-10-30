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

// Zero stream credit can be rejected immediately or queued by a transport that ignores
// waitUntilAvailable. Once a slot returns, observe both resets and any unwanted bytes.
function limited(mode: "refuse" | "queue" | "open") {
	const slot = Promise.withResolvers<WebTransportBidirectionalStream>();
	const created = Promise.withResolvers<void>();
	const aborted = Promise.withResolvers<unknown>();
	const cancelled = Promise.withResolvers<unknown>();
	const written: Uint8Array[] = [];
	const options: unknown[] = [];
	const stream = {
		readable: new ReadableStream<Uint8Array>({ cancel: cancelled.resolve }),
		writable: new WritableStream<Uint8Array>({
			abort: aborted.resolve,
			write: (bytes) => {
				written.push(new Uint8Array(bytes));
			},
		}),
	};
	const quic = {
		closed: new Promise(() => {}),
		createBidirectionalStream: (opts: unknown) => {
			options.push(opts);
			created.resolve();
			if (mode === "refuse") return Promise.reject(new DOMException("stream limit reached", "NetworkError"));
			return mode === "open" ? Promise.resolve(stream) : slot.promise;
		},
	} as unknown as WebTransport;
	return {
		quic,
		created: created.promise,
		aborted: aborted.promise,
		cancelled: cancelled.promise,
		written,
		options,
		release: () => slot.resolve(stream),
	};
}

for (const protocol of ["lite", "ietf"] as const) {
	for (const mode of ["refuse", "queue", "open"] as const) {
		test(`${protocol}: ${mode} has one fatal request deadline`, async () => {
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
				if (mode === "refuse") expect(track.closed.peek()?.name).toBe("NoStreamCredit");
				await clock.advance(Nano.fromMilli(Milli(9_999)));
				if (mode !== "refuse") expect(track.closed.peek()).toBeUndefined();
				await clock.advance(Nano.fromMilli(Milli(1)));
				const failure = await track.closed;
				if (mode === "open") {
					expect(failure).toBeInstanceOf(StreamError);
					expect((failure as StreamError).code).toBe(StreamCode.ControlTimeout);
				} else {
					expect(failure?.name).toBe("NoStreamCredit");
				}
				expect(transport.options).toEqual([{ sendOrder: undefined, waitUntilAvailable: false }]);
				if (mode === "queue") {
					transport.release();
					await transport.aborted;
					await transport.cancelled;
					expect(transport.written).toEqual([]);
				}
				if (mode === "open") {
					await transport.aborted;
					await transport.cancelled;
				}
				await clock.advance(Nano.fromMilli(Milli(30_000)));
				expect(transport.options).toHaveLength(1);
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
	for (const mode of ["refuse", "queue", "open"] as const) {
		test(`lite standalone ${operation}: ${mode} has a fatal deadline`, async () => {
			const clock = new Manual();
			const installed = clock.install();
			const transport = limited(mode);
			const subscriber = new LiteSubscriber(transport.quic, Lite.DRAFT_07, HopSchema.parse(1n));
			let failure: unknown;
			const request = (
				operation === "info"
					? subscriber.resolveTrackInfo(Path.from("room"), "video")
					: subscriber.fetchGroup(Path.from("room"), "video", 0)
			).catch((err: unknown) => {
				failure = err;
			});
			try {
				await transport.created;
				await clock.advance(Nano.zero);
				if (mode === "refuse") expect((failure as Error).name).toBe("NoStreamCredit");
				await clock.advance(Nano.fromMilli(Milli(10_000)));
				await request;
				if (mode === "open") expect((failure as StreamError).code).toBe(StreamCode.ControlTimeout);
				else expect((failure as Error).name).toBe("NoStreamCredit");
				if (mode === "queue") {
					transport.release();
					await transport.aborted;
					await transport.cancelled;
					expect(transport.written).toEqual([]);
				}
				expect(transport.options).toEqual([{ sendOrder: undefined, waitUntilAvailable: false }]);
			} finally {
				subscriber.close();
				installed.close();
			}
		});
	}
}

for (const protocol of ["lite", "ietf"] as const) {
	for (const mode of ["refuse", "queue", "open"] as const) {
		test(`${protocol} discovery: ${mode} bounds only the create`, async () => {
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
				if (mode === "open") expect(announced.closed.peek()).toBeUndefined();
				else {
					expect((await announced.closed)?.name).toBe("NoStreamCredit");
					if (mode === "queue") {
						transport.release();
						await transport.aborted;
						await transport.cancelled;
						expect(transport.written).toEqual([]);
					}
				}
				expect(transport.options).toEqual([{ sendOrder: undefined, waitUntilAvailable: false }]);
			} finally {
				announced.close();
				if (subscriber instanceof LiteSubscriber) subscriber.close();
				installed.close();
			}
		});
	}
}

for (const protocol of ["lite", "ietf"] as const) {
	test(`${protocol}: credit granted in the deadline task cannot send a late request`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		const transport = limited("queue");
		const subscriber =
			protocol === "lite"
				? new LiteSubscriber(transport.quic, Lite.DRAFT_07, HopSchema.parse(1n))
				: new IetfSubscriber({ session: new NativeSession(transport.quic, Ietf.DRAFT_18, true) });
		const broadcast = subscriber.consume(Path.from("room"));
		const track = broadcast.track("video").subscribe();
		try {
			await transport.created;
			clock.jump(Nano.fromMilli(Milli(10_000)));
			transport.release();
			await clock.advance(Nano.zero);
			expect((await track.closed)?.name).toBe("NoStreamCredit");
			await transport.aborted;
			expect(transport.written).toEqual([]);
		} finally {
			track.close();
			broadcast.close();
			if (subscriber instanceof LiteSubscriber) subscriber.close();
			installed.close();
		}
	});
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
