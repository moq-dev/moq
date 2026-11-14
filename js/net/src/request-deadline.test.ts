import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { SessionCode, SessionError } from "./error.ts";
import { toHop } from "./hop.ts";
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
	const writing = Promise.withResolvers<void>();
	const written: Uint8Array[] = [];
	const options: unknown[] = [];
	const stream = () => ({
		readable: new ReadableStream<Uint8Array>(),
		writable: new WritableStream<Uint8Array>({
			abort: aborted.resolve,
			write: (bytes) => {
				written.push(new Uint8Array(bytes));
				writing.resolve();
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
		writing: writing.promise,
		written,
		options,
		release: () => slot.resolve(),
	};
}

const HOUR = Duration.millis(3_600_000);

// A publisher may hold its answer, so a request waits until its caller leaves, as in Rust.
// Cancellation resets opened streams and gives up queued opens without spending their credit.
for (const protocol of ["lite", "ietf"] as const) {
	for (const mode of ["queue", "open"] as const) {
		test(`${protocol}: ${mode} subscribe waits until its subscriber leaves`, async () => {
			const clock = new Manual();
			const installed = clock.install();
			const transport = limited(mode);
			const subscriber =
				protocol === "lite"
					? new LiteSubscriber(transport.quic, Lite.DRAFT_07, toHop(1n))
					: new IetfSubscriber({ session: new NativeSession(transport.quic, Ietf.DRAFT_18, true) });
			const broadcast = subscriber.consume(Path.from("room"));
			const track = broadcast.track("video").subscribe();
			try {
				await transport.created;
				await clock.advance(Duration.ZERO);
				await clock.advance(HOUR);
				expect(track.closed.peek()).toBeUndefined();
				track.close();
				if (mode === "queue") {
					transport.release();
					await clock.advance(Duration.ZERO);
					expect(transport.written).toEqual([]);
				} else {
					await transport.aborted;
				}
				// A lite subscription opens its SUBSCRIBE alongside its TRACK.
				const opened = protocol === "lite" ? 2 : 1;
				expect(transport.options).toEqual(Array.from({ length: opened }, () => ({ waitUntilAvailable: true })));
			} finally {
				track.close();
				broadcast.close();
				if (subscriber instanceof LiteSubscriber) subscriber.close();
				installed.close();
			}
		});
	}
}

for (const protocol of ["lite", "ietf"] as const) {
	test(`${protocol}: an unanswered track info waits until its caller leaves`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		const transport = limited("open");
		const subscriber =
			protocol === "lite"
				? new LiteSubscriber(transport.quic, Lite.DRAFT_07, toHop(1n))
				: new IetfSubscriber({ session: new NativeSession(transport.quic, Ietf.DRAFT_18, true) });
		const broadcast = subscriber.consume(Path.from("room"));
		const cancel = new AbortController();
		let failure: unknown;
		const info = broadcast
			.track("video")
			.info({ signal: cancel.signal })
			.catch((err: unknown) => {
				failure = err;
			});
		try {
			await transport.created;
			await clock.advance(HOUR);
			expect(failure).toBeUndefined();
			const left = new Error("left");
			cancel.abort(left);
			await info;
			expect(failure).toBe(left);
			await transport.aborted;
		} finally {
			broadcast.close();
			if (subscriber instanceof LiteSubscriber) subscriber.close();
			installed.close();
		}
	});
}

for (const operation of ["info", "fetch"] as const) {
	for (const mode of ["queue", "open"] as const) {
		test(`lite standalone ${operation}: ${mode} waits until its caller leaves`, async () => {
			const clock = new Manual();
			const installed = clock.install();
			const transport = limited(mode);
			const subscriber = new LiteSubscriber(transport.quic, Lite.DRAFT_07, toHop(1n));
			const cancel = new AbortController();
			let failure: unknown;
			const request = (
				operation === "info"
					? subscriber.resolveTrackInfo(Path.from("room"), "video", undefined, cancel.signal)
					: subscriber.fetchGroup(Path.from("room"), "video", 0n, { signal: cancel.signal })
			).catch((err: unknown) => {
				failure = err;
			});
			try {
				await transport.created;
				await clock.advance(Duration.ZERO);
				await clock.advance(HOUR);
				expect(failure).toBeUndefined();
				const left = new Error("left");
				cancel.abort(left);
				await request;
				expect(failure).toBe(left);
				// Credit granted once the caller has left can't send the request.
				await clock.advance(Duration.ZERO);
				if (mode === "queue") {
					transport.release();
					await clock.advance(Duration.ZERO);
					expect(transport.written).toEqual([]);
				} else {
					await transport.aborted;
				}
				// A fetch opens its FETCH stream alongside the TRACK stream.
				const opened = transport.options as { sendOrder?: number; waitUntilAvailable: boolean }[];
				expect(opened).toHaveLength(operation === "info" ? 1 : 2);
				for (const open of opened) expect(open.waitUntilAvailable).toBe(true);
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
					? new LiteSubscriber(transport.quic, Lite.DRAFT_07, toHop(1n))
					: new IetfSubscriber({ session: new NativeSession(transport.quic, Ietf.DRAFT_18, true) });
			const announced = subscriber.routes();
			try {
				await transport.created;
				await clock.advance(Duration.ZERO);
				await clock.advance(Duration.millis(30_000));
				expect(announced.closed.peek()).toBeUndefined();
				if (mode === "queue") {
					expect(transport.written).toEqual([]);
					transport.release();
					await clock.advance(Duration.ZERO);
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
	test(`${protocol}: credit granted as the subscriber leaves cannot send a late request`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		const transport = limited("queue");
		const subscriber =
			protocol === "lite"
				? new LiteSubscriber(transport.quic, Lite.DRAFT_07, toHop(1n))
				: new IetfSubscriber({ session: new NativeSession(transport.quic, Ietf.DRAFT_18, true) });
		const broadcast = subscriber.consume(Path.from("room"));
		const track = broadcast.track("video").subscribe();
		try {
			await transport.created;
			track.close();
			transport.release();
			await clock.advance(Duration.ZERO);
			expect(transport.written).toEqual([]);
			const next = broadcast.track("next").subscribe();
			await transport.writing;
			next.close();
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
				? new LiteSubscriber(quic, Lite.DRAFT_07, toHop(1n))
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
