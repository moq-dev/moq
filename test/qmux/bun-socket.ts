/** Accept a QMux session from `Bun.serve`. @module */
import type { WebSocketStreamLike } from "@moq/qmux";
import type { ServerWebSocket } from "bun";

type OpenEvent = Awaited<WebSocketStreamLike["opened"]>;
type CloseEvent = Awaited<WebSocketStreamLike["closed"]>;
type CloseInfo = NonNullable<Parameters<WebSocketStreamLike["close"]>[0]>;

/** Bun's `ServerWebSocket` is handler-based rather than a `WebSocket`, so it
 *  can't be adopted directly. Presenting it as a `WebSocketStreamLike` is the
 *  escape hatch every non-standard host has, and what makes `Session.accept`
 *  usable from `Bun.serve` at all. */
export class BunSocketStream implements WebSocketStreamLike {
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

	/** A message arrived on the socket. */
	push(data: string | Buffer): void {
		// Bun hands back a Buffer; copy to a plain Uint8Array view of just this
		// message rather than passing the pooled buffer through.
		this.#incoming.enqueue(typeof data === "string" ? data : new Uint8Array(data));
	}

	/** The socket closed. */
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
