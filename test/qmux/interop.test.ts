// The TypeScript qmux peer against the Rust one, per draft, in both roles.
// The Rust side is the in-tree qmux crate's interop examples, built by `just test qmux`.
import { expect, test } from "bun:test";
import { join } from "node:path";
import Session, { selectSubprotocol, type Version, type WebSocketStreamLike } from "@moq/qmux";
import type { ServerWebSocket } from "bun";

type OpenEvent = Awaited<WebSocketStreamLike["opened"]>;
type CloseEvent = Awaited<WebSocketStreamLike["closed"]>;
type CloseInfo = NonNullable<Parameters<WebSocketStreamLike["close"]>[0]>;

const BIN = process.env.QMUX_INTEROP_BIN;
if (!BIN) throw new Error("QMUX_INTEROP_BIN is unset; run `just test qmux`");

const PROTOCOL = "qmux-interop";
const PAYLOAD_LEN = 1_200_000;
const CLIENT_SEED = 17;
const SERVER_SEED = 29;
const OPERATION_TIMEOUT_MS = 15_000;
const VERSIONS = ["qmux-02", "qmux-01", "qmux-00"] as const;

// Small receive windows force the Rust sender to observe and react to
// MAX_DATA/MAX_STREAM_DATA updates during the large transfer.
const CONFIG = {
	receiveWindow: 64n * 1024n,
	streamReceiveWindow: 32n * 1024n,
};

function payload(seed: number): Uint8Array {
	const bytes = new Uint8Array(PAYLOAD_LEN);
	for (let index = 0; index < bytes.byteLength; index++) {
		bytes[index] = (index * 31 + seed) % 251;
	}
	return bytes;
}

function expectPayload(actual: Uint8Array, seed: number): void {
	expect(actual.byteLength).toBe(PAYLOAD_LEN);
	for (let index = 0; index < actual.byteLength; index++) {
		if (actual[index] !== (index * 31 + seed) % 251) {
			throw new Error(`payload mismatch at byte ${index}`);
		}
	}
}

async function withTimeout<T>(label: string, promise: Promise<T>): Promise<T> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		return await Promise.race([
			promise,
			new Promise<never>((_, reject) => {
				timer = setTimeout(() => reject(new Error(`timed out: ${label}`)), OPERATION_TIMEOUT_MS);
			}),
		]);
	} finally {
		if (timer !== undefined) clearTimeout(timer);
	}
}

async function readLine(stream: ReadableStream<Uint8Array>): Promise<string> {
	const reader = stream.getReader();
	const decoder = new TextDecoder();
	let buffered = "";
	try {
		while (true) {
			const { value, done } = await reader.read();
			if (done) throw new Error("Rust interop server exited before reporting its URL");
			buffered += decoder.decode(value, { stream: true });
			const newline = buffered.indexOf("\n");
			if (newline !== -1) return buffered.slice(0, newline).trim();
		}
	} finally {
		reader.releaseLock();
	}
}

async function readAll(stream: ReadableStream<Uint8Array>): Promise<Uint8Array> {
	return new Uint8Array(await new Response(stream).arrayBuffer());
}

/** Rust sends a datagram first, TypeScript answers; QMux00 has no DATAGRAM. */
async function exchangeDatagrams(session: Session, version: Version): Promise<void> {
	if (version === "qmux-00") {
		expect(session.datagrams.maxDatagramSize).toBe(0);
		return;
	}

	expect(session.datagrams.maxDatagramSize).toBeGreaterThan(0);
	const datagramReader = session.datagrams.readable.getReader();
	const fromRust = await withTimeout("receiving the Rust datagram", datagramReader.read());
	datagramReader.releaseLock();
	expect(fromRust.done).toBe(false);
	expect(new TextDecoder().decode(fromRust.value)).toBe("rust-datagram");

	const datagramWriter = session.datagrams.writable.getWriter();
	await datagramWriter.write(new TextEncoder().encode("typescript-datagram"));
	datagramWriter.releaseLock();
}

/** Close with the code the Rust peer checks, then require it to exit cleanly. */
async function finish(session: Session, peer: Bun.Subprocess, stderr: Promise<string>): Promise<void> {
	session.close({ closeCode: 42, reason: "interop complete" });
	expect(await withTimeout("closing the TypeScript session", session.closed)).toEqual({
		closeCode: 42,
		reason: "interop complete",
	});

	const exitCode = await withTimeout("waiting for the Rust peer to exit", peer.exited);
	if (exitCode !== 0) {
		throw new Error(`Rust peer exited with ${exitCode}:\n${await stderr}`);
	}
}

/** Kill the Rust peer and attach its stderr to the failure. */
async function fail(error: unknown, peer: Bun.Subprocess, stderr: Promise<string>): Promise<never> {
	peer.kill();
	await peer.exited;
	throw new Error(`${error instanceof Error ? error.message : String(error)}\n${await stderr}`);
}

/** The TypeScript client dials the Rust server, which accepts only `version`. */
async function runClientVersion(version: Version): Promise<void> {
	const server = Bun.spawn([join(BIN as string, "interop-server"), version], { stdout: "pipe", stderr: "pipe" });
	const stderr = new Response(server.stderr).text();

	try {
		const url = await withTimeout("starting the Rust interop server", readLine(server.stdout));
		expect(url).toStartWith("ws://127.0.0.1:");

		const session = new Session(url, {
			protocols: [PROTOCOL],
			versions: { [PROTOCOL]: version },
			requireProtocol: true,
			config: CONFIG,
		});
		await withTimeout("opening the QMux session", session.ready);
		expect(session.protocol).toBe(PROTOCOL);

		const outgoing = await withTimeout(
			"creating the client unidirectional stream",
			session.createUnidirectionalStream(),
		);
		const outgoingWriter = outgoing.getWriter();
		await withTimeout("writing the client flow-control payload", outgoingWriter.write(payload(CLIENT_SEED)));
		await withTimeout("finishing the client unidirectional stream", outgoingWriter.close());

		const incomingReader = session.incomingUnidirectionalStreams.getReader();
		const incoming = await withTimeout("accepting the server unidirectional stream", incomingReader.read());
		incomingReader.releaseLock();
		if (incoming.value === undefined) throw new Error("server unidirectional stream was missing");
		expectPayload(
			await withTimeout("reading the server flow-control payload", readAll(incoming.value)),
			SERVER_SEED,
		);

		const bidi = await withTimeout("creating the bidirectional stream", session.createBidirectionalStream());
		const bidiWriter = bidi.writable.getWriter();
		await withTimeout(
			"writing the bidirectional request",
			bidiWriter.write(new TextEncoder().encode(`ping:${version}`)),
		);
		await withTimeout("finishing the bidirectional request", bidiWriter.close());
		const bidiResponse = await withTimeout("reading the bidirectional response", readAll(bidi.readable));
		expect(new TextDecoder().decode(bidiResponse)).toBe(`pong:${version}`);

		await exchangeDatagrams(session, version);
		await finish(session, server, stderr);
	} catch (error) {
		await fail(error, server, stderr);
	}
}

test("A TypeScript client interoperates with the Rust WebSocket server across supported drafts", async () => {
	for (const version of VERSIONS) {
		await runClientVersion(version);
	}
}, 120_000);

/** Bun's `ServerWebSocket` is handler-based rather than a `WebSocket`, so it
 *  can't be adopted directly. Presenting it as a `WebSocketStreamLike` is the
 *  escape hatch every non-standard host has, and what makes `Session.accept`
 *  usable from `Bun.serve` at all. */
class BunSocketStream implements WebSocketStreamLike {
	readonly url = "";
	readonly protocol: string;
	readonly opened: Promise<OpenEvent>;
	readonly closed: Promise<CloseEvent>;
	#opened = Promise.withResolvers<OpenEvent>();
	#closed = Promise.withResolvers<CloseEvent>();
	#incoming!: ReadableStreamDefaultController<Uint8Array | string>;
	#socket?: ServerWebSocket<{ stream: BunSocketStream }>;

	readonly #readable = new ReadableStream<Uint8Array | string>({
		start: (controller) => {
			this.#incoming = controller;
		},
	});

	constructor(protocol: string) {
		this.protocol = protocol;
		this.opened = this.#opened.promise;
		this.closed = this.#closed.promise;
		this.opened.catch(() => {});
	}

	/** The upgrade completed: hand the session its streams. */
	attach(socket: ServerWebSocket<{ stream: BunSocketStream }>): void {
		this.#socket = socket;
		const writable = new WritableStream<Uint8Array | string>({
			write: (chunk) => {
				socket.send(chunk);
			},
			close: () => socket.close(),
			abort: () => socket.close(),
		});
		this.#opened.resolve({ readable: this.#readable, writable, extensions: "", protocol: this.protocol });
	}

	push(data: string | Buffer): void {
		// Bun hands back a Buffer; copy to a plain Uint8Array view of just this
		// message rather than passing the pooled buffer through.
		this.#incoming.enqueue(typeof data === "string" ? data : new Uint8Array(data));
	}

	finish(closeCode: number, reason: string): void {
		try {
			this.#incoming.close();
		} catch {}
		this.#opened.reject(new Error("socket closed before opening"));
		this.#closed.resolve({ closeCode, reason });
	}

	close(info: CloseInfo = {}): void {
		this.#socket?.close(info.closeCode, info.reason);
	}
}

/** The mirror of `runClientVersion`: the TypeScript side accepts and the Rust
 *  client dials, so `Session.accept` owns the server half of the stream-id space. */
async function runServerVersion(version: Version): Promise<void> {
	const sessions = Promise.withResolvers<Session>();

	const server = Bun.serve<{ stream: BunSocketStream }>({
		port: 0,
		hostname: "127.0.0.1",
		fetch(req, server) {
			const protocol = selectSubprotocol(req.headers.get("sec-websocket-protocol"), {
				protocols: [PROTOCOL],
				versions: { [PROTOCOL]: version },
				requireProtocol: true,
			});
			if (!protocol) return new Response("no supported protocol", { status: 400 });

			const stream = new BunSocketStream(protocol);
			// Bun's socket doesn't report the negotiated subprotocol, so the session
			// is told directly; the wire format is derived from it.
			sessions.resolve(Session.accept(stream, { protocol, config: CONFIG }));
			if (server.upgrade(req, { data: { stream }, headers: { "sec-websocket-protocol": protocol } })) return;
			return new Response("upgrade failed", { status: 500 });
		},
		websocket: {
			open: (ws) => ws.data.stream.attach(ws),
			message: (ws, message) => ws.data.stream.push(message),
			close: (ws, code, reason) => ws.data.stream.finish(code, reason),
		},
	});

	const client = Bun.spawn(
		[join(BIN as string, "interop-client"), `ws://127.0.0.1:${server.port}/interop`, version],
		{
			stdout: "pipe",
			stderr: "pipe",
		},
	);
	const stderr = new Response(client.stderr).text();

	try {
		const session = await withTimeout("accepting the Rust client's session", sessions.promise);
		await withTimeout("opening the QMux session", session.ready);
		expect(session.protocol).toBe(PROTOCOL);

		const incomingReader = session.incomingUnidirectionalStreams.getReader();
		const incoming = await withTimeout("accepting the client unidirectional stream", incomingReader.read());
		incomingReader.releaseLock();
		if (incoming.value === undefined) throw new Error("client unidirectional stream was missing");
		expectPayload(
			await withTimeout("reading the client flow-control payload", readAll(incoming.value)),
			CLIENT_SEED,
		);

		const outgoing = await withTimeout(
			"creating the server unidirectional stream",
			session.createUnidirectionalStream(),
		);
		const outgoingWriter = outgoing.getWriter();
		await withTimeout("writing the server flow-control payload", outgoingWriter.write(payload(SERVER_SEED)));
		await withTimeout("finishing the server unidirectional stream", outgoingWriter.close());

		const bidiReader = session.incomingBidirectionalStreams.getReader();
		const bidi = await withTimeout("accepting the bidirectional stream", bidiReader.read());
		bidiReader.releaseLock();
		if (bidi.value === undefined) throw new Error("bidirectional stream was missing");
		const request = await withTimeout("reading the bidirectional request", readAll(bidi.value.readable));
		expect(new TextDecoder().decode(request)).toBe(`ping:${version}`);
		const bidiWriter = bidi.value.writable.getWriter();
		await withTimeout(
			"writing the bidirectional response",
			bidiWriter.write(new TextEncoder().encode(`pong:${version}`)),
		);
		await withTimeout("finishing the bidirectional response", bidiWriter.close());

		await exchangeDatagrams(session, version);
		await finish(session, client, stderr);
	} catch (error) {
		await fail(error, client, stderr);
	} finally {
		server.stop(true);
	}
}

test("A TypeScript server interoperates with the Rust WebSocket client across supported drafts", async () => {
	for (const version of VERSIONS) {
		await runServerVersion(version);
	}
}, 120_000);
