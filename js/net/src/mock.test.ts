import { expect, test } from "bun:test";
import { StreamCode, toTransport } from "./error.ts";
import { createMockTransportPair } from "./mock.ts";

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
