import { expect, test } from "bun:test";
import { Duration, Elapsed, Instant, now, sleep, timeout } from "@moq/clock";
import { StreamCode, toTransport } from "./error.ts";
import { createMockTransportPair, manual } from "./mock.ts";

const ms = (n: number) => Duration.millis(n);

async function pair() {
	const { client, server } = createMockTransportPair();
	const local = await client.createBidirectionalStream();
	const { value: remote } = await server.incomingBidirectionalStreams.getReader().read();
	if (!remote) throw new Error("no incoming stream");
	return { local, remote };
}

/** What a read sees once the stream fails. */
function readError(readable: ReadableStream<Uint8Array>): Promise<unknown> {
	return readable
		.getReader()
		.read()
		.catch((e: unknown) => e);
}

test("a reset reaches the peer as its code alone, like a real wire", async () => {
	const { local, remote } = await pair();
	await local.writable.abort(new Error("local detail"));
	const err = await readError(remote.readable);
	expect(err).toMatchObject({ source: "stream", streamErrorCode: 0 });
	expect((err as Error).message).not.toContain("local detail");
});

test("a reset keeps the code its reason carries", async () => {
	const { local, remote } = await pair();
	await local.writable.abort(toTransport(StreamCode.Cancel, "cancel"));
	const err = await readError(remote.readable);
	expect(err).toMatchObject({ source: "stream", streamErrorCode: StreamCode.Cancel });
});

test("a stop reaches the writer as its code alone", async () => {
	const { local, remote } = await pair();
	await remote.readable.cancel(new Error("local detail"));
	const err = await local.writable.getWriter().closed.catch((e: unknown) => e);
	expect(err).toMatchObject({ source: "stream", streamErrorCode: 0 });
	expect((err as Error).message).not.toContain("local detail");
});

test("a write parked on a full peer gives up when the peer stops", async () => {
	const { local, remote } = await pair();
	const writer = local.writable.getWriter();
	// Past both queues, so the last write parks on backpressure.
	const writes = Array.from({ length: 600 }, () => writer.write(new Uint8Array(1)).catch((e: unknown) => e));
	// The peer queues 256 chunks. The write before the 256th settles just as the 256th starts,
	// which fills the queue and parks in flight.
	await writes[254];
	await remote.readable.cancel(toTransport(StreamCode.Cancel, "cancel"));
	const last = await writes[writes.length - 1];
	expect(last).toMatchObject({ source: "stream", streamErrorCode: StreamCode.Cancel });
});

test("drive delivers a queued round trip before its deadline and stops at the deadline the work then meets", async () => {
	using clock = manual();
	const { local, remote } = await pair();
	let late = false;
	void sleep(ms(60_000)).then(() => {
		late = true;
	});
	// The peer answers only once the request reaches it, so the response is a second delivery.
	void (async () => {
		const { value } = await remote.readable.getReader().read();
		if (value) await remote.writable.getWriter().write(value);
	})();
	const work = (async () => {
		await local.writable.getWriter().write(Uint8Array.of(7));
		const { value } = await timeout(ms(1000), local.readable.getReader().read());
		await sleep(ms(10));
		return { value, at: now() };
	})();
	expect(await clock.drive(work)).toEqual({ value: Uint8Array.of(7), at: Instant.ZERO.add(ms(10)) });
	expect(late).toBe(false);
});

test("drive times out a read with nothing in flight at its deadline", async () => {
	using clock = manual();
	const { local } = await pair();
	const read = timeout(ms(1000), local.readable.getReader().read());
	expect(await clock.drive(read.catch((e: unknown) => e))).toBeInstanceOf(Elapsed);
	expect(clock.now()).toEqual(Instant.ZERO.add(ms(1000)));
});

test("drive reaches a deadline while deliveries keep arriving", async () => {
	using clock = manual();
	const { local, remote } = await pair();
	// A peer that refills the stream's queue on every host turn, so a delivery is always pending.
	let delivered = 0;
	const reader = remote.readable.getReader();
	void (async () => {
		while (!(await reader.read()).done) delivered++;
	})();
	const writer = local.writable.getWriter();
	let sending = true;
	const send = () => {
		if (!sending) return;
		while ((writer.desiredSize ?? 0) > 0) void writer.write(Uint8Array.of(1));
		setImmediate(send);
	};
	send();
	try {
		await clock.drive(sleep(ms(1000)));
		expect(clock.now()).toEqual(Instant.ZERO.add(ms(1000)));
		expect(delivered).toBeGreaterThan(0);
	} finally {
		sending = false;
	}
});
