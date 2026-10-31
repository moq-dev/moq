import { SessionCode, SessionError } from "../error.ts";
import { type OpenOptions, Stream } from "../stream.ts";
import type { IetfVersion } from "./version.ts";

/** Opens request streams and allocates request IDs. Implemented by {@link NativeSession}; tests substitute their own. */
export interface Session {
	openBi(options?: Partial<OpenOptions>): Stream | Promise<Stream>;
	acceptBi(): Promise<Stream | undefined>;
	nextRequestId(signal?: AbortSignal): Promise<bigint | undefined>;
	/** Check a request ID the peer used, closing the session on a wrong-parity one. */
	checkRequestId(id: bigint): void;
	close(): void;
	readonly version: IetfVersion;
	/** Whether we opened the session. */
	readonly client: boolean;
}

/** A thin wrapper around WebTransport: each request gets its own bidi stream. */
export class NativeSession implements Session {
	#quic: WebTransport;
	// moq-transport reserves even request IDs for the client and odd for the server,
	// so the two peers' ID spaces never overlap.
	#requestId: bigint;
	readonly version: IetfVersion;
	readonly client: boolean;

	constructor(quic: WebTransport, version: IetfVersion, client: boolean) {
		this.#quic = quic;
		this.version = version;
		this.client = client;
		this.#requestId = client ? 0n : 1n;
	}

	checkRequestId(id: bigint): void {
		try {
			checkPeerRequestId(id, this.client);
		} catch (err) {
			if (err instanceof SessionError) this.#quic.close({ closeCode: err.code, reason: err.message });
			throw err;
		}
	}

	async openBi(options?: Partial<OpenOptions>): Promise<Stream> {
		return Stream.open(this.#quic, { ...options, version: this.version });
	}

	async acceptBi(): Promise<Stream | undefined> {
		return Stream.accept(this.#quic, this.version);
	}

	async nextRequestId(signal?: AbortSignal): Promise<bigint | undefined> {
		signal?.throwIfAborted();
		const id = this.#requestId;
		this.#requestId += 2n;
		return id;
	}

	/** Closes the underlying WebTransport session. */
	close() {
		this.#quic.close();
	}
}

/**
 * Check a request ID the peer used: it must have the peer's parity, odd from a server.
 *
 * Request streams can arrive out of order, and a reused ID is accepted too: each request owns
 * its stream, so a reuse only confuses the reusing peer's own joining FETCHes, and remembering
 * every skipped ID would cost memory the peer chooses. Mirrors moq-net.
 */
export function checkPeerRequestId(id: bigint, client: boolean): void {
	// No peer reaches 2^62 requests; refusing past it matches moq-net, whose counter would overflow.
	if (id >= 1n << 62n || id % 2n !== (client ? 1n : 0n)) throw new SessionError(SessionCode.InvalidRequestId);
}
