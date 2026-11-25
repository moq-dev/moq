import { expect, test } from "bun:test";
import * as Instance from "../instance.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { ownClaim } from "./announce.ts";
import { AnnounceStream } from "./announce_stream.ts";
import { Version } from "./version.ts";

const version = Version.DRAFT_07;

// Finishing right behind a send, before that send opened the stream, still finishes the stream
// the send opened, so the peer sees the FIN.
test("finishing behind an unwritten send finishes the stream it opens", async () => {
	const { client, server } = createMockTransportPair();
	try {
		const stream = AnnounceStream.of(client, version);
		const sent = stream.send({ status: "ok" });
		const finished = stream.finish();
		const incoming = server.incomingUnidirectionalStreams.getReader();
		const { value } = await incoming.read();
		if (!value) throw new Error("no stream");
		const reader = value.getReader();
		for (;;) {
			const { done } = await reader.read();
			if (done) break;
		}
		await sent;
		await finished;
	} finally {
		client.close();
		server.close();
	}
});

// A send that fails loses what this endpoint said: `broken` reports it, and a stream that never
// failed never settles it.
test("a failed send breaks the stream", async () => {
	const { client, server } = createMockTransportPair();
	try {
		const stream = AnnounceStream.of(client, version);
		let broken = false;
		void stream.broken.then(() => {
			broken = true;
		});
		await stream.send({ status: "ok" });
		await Promise.resolve();
		expect(broken).toBe(false);

		client.close();
		await expect(stream.send({ status: "ok" })).rejects.toThrow();
		await stream.broken;
		expect(broken).toBe(true);
	} finally {
		server.close();
	}
});

// Each uni stream `quic` opens from now on delivers its bytes and FIN, but its close never
// completes: the peer never acknowledges the FIN.
function holdFins(quic: WebTransport) {
	const create = quic.createUnidirectionalStream.bind(quic);
	quic.createUnidirectionalStream = (async (options?: WebTransportSendStreamOptions) => {
		const inner = (await create(options)).getWriter();
		return new WritableStream<Uint8Array>({
			write: (chunk) => inner.write(chunk),
			close: () => {
				void inner.close();
				return new Promise<void>(() => {});
			},
		});
	}) as WebTransport["createUnidirectionalStream"];
}

// Reads the peer's one incoming uni stream to its FIN.
async function readToFin(quic: WebTransport) {
	const { value } = await quic.incomingUnidirectionalStreams.getReader().read();
	if (!value) throw new Error("no stream");
	const reader = value.getReader();
	while (!(await reader.read()).done) {}
}

// A stream that only asked the peer for prefixes ends nothing with its FIN, so leaving does not
// wait for the peer to acknowledge it.
test("finishing a stream that sent no claim waits on no acknowledgement", async () => {
	const { client, server } = createMockTransportPair();
	try {
		holdFins(client);
		const stream = AnnounceStream.of(client, version);
		await stream.send({
			status: "request",
			more: false,
			remove: [],
			add: [{ prefix: Path.from("a"), hidden: false }],
		});
		await Promise.all([stream.finish(), readToFin(server)]);
	} finally {
		client.close();
		server.close();
	}
});

// A claim ends only with the stream's FIN, so leaving waits until the peer acknowledges it.
test("finishing a stream that sent a claim waits for its acknowledgement", async () => {
	const { client, server } = createMockTransportPair();
	try {
		holdFins(client);
		const stream = AnnounceStream.of(client, version);
		await stream.send({
			status: "start",
			suffix: Path.from("a"),
			instance: Instance.parse("a"),
			claim: ownClaim(0n),
		});
		let finished = false;
		void stream.finish().then(() => {
			finished = true;
		});
		await readToFin(server);
		await new Promise(setImmediate);
		expect(finished).toBe(false);
	} finally {
		client.close();
		server.close();
	}
});

// Each uni stream `quic` opens from now on fails its close as a peer's STOP_SENDING with `code` does.
function stopOnClose(quic: WebTransport, code: number) {
	const create = quic.createUnidirectionalStream.bind(quic);
	quic.createUnidirectionalStream = (async (options?: WebTransportSendStreamOptions) => {
		const inner = (await create(options)).getWriter();
		return new WritableStream<Uint8Array>({
			write: (chunk) => inner.write(chunk),
			close: () =>
				Promise.reject(Object.assign(new Error("stopped"), { source: "stream", streamErrorCode: code })),
		});
	}) as WebTransport["createUnidirectionalStream"];
}

// A peer stopping a claim's stream without a code of its own is leaving as this side does, which
// owes it nothing more; a stop with a code still fails the finish.
for (const [code, leaves] of [
	[0, true],
	[7, false],
] as const) {
	test(`finishing a claim's stream the peer stops with code ${code} ${leaves ? "resolves" : "rejects"}`, async () => {
		const { client, server } = createMockTransportPair();
		try {
			stopOnClose(client, code);
			const stream = AnnounceStream.of(client, version);
			await stream.send({
				status: "start",
				suffix: Path.from("a"),
				instance: Instance.parse("a"),
				claim: ownClaim(0n),
			});
			const finished = stream.finish();
			if (leaves) await finished;
			else await expect(finished).rejects.toThrow();
		} finally {
			client.close();
			server.close();
		}
	});
}
