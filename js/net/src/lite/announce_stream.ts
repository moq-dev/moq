// Lite-07: this endpoint's one unidirectional Announce Stream, on which it sends everything it
// says about discovery and routing in order: as a subscriber the edits of the prefixes it asks
// for, and as a publisher the routes and claims answering what its peer asks for.

import { Writer } from "../stream.ts";
import { type AnnounceBroadcast, encodeAnnounceBroadcast } from "./announce.ts";
import { DataType } from "./stream.ts";
import type { Version } from "./version.ts";

const streams = new WeakMap<WebTransport, AnnounceStream>();

/** A session's outgoing Announce Stream, opened with the first message it sends. @internal */
export class AnnounceStream {
	#quic: WebTransport;
	#version: Version;
	#writer?: Promise<Writer>;
	// The last write queued, which the next one follows.
	#tail: Promise<void> = Promise.resolve();
	#finished = false;

	private constructor(quic: WebTransport, version: Version) {
		this.#quic = quic;
		this.#version = version;
	}

	/** The session's one stream, shared by the half that asks and the half that answers. */
	static of(quic: WebTransport, version: Version): AnnounceStream {
		let stream = streams.get(quic);
		if (!stream) {
			stream = new AnnounceStream(quic, version);
			streams.set(quic, stream);
		}
		return stream;
	}

	#open(): Promise<Writer> {
		this.#writer ??= (async () => {
			const writer = await Writer.open(this.#quic, { version: this.#version });
			await writer.u53(DataType.Announce);
			return writer;
		})();
		return this.#writer;
	}

	/**
	 * Send `msgs` back to back, after everything sent before them, so nothing lands between a
	 * route's claims and the floor closing their batch. Rejects once the stream finished or failed.
	 */
	send(...msgs: AnnounceBroadcast[]): Promise<void> {
		if (this.#finished) return Promise.reject(new Error("the Announce Stream finished"));
		const sent = this.#tail.then(async () => {
			const writer = await this.#open();
			for (const msg of msgs) await encodeAnnounceBroadcast(writer, msg, this.#version);
		});
		this.#tail = sent.catch(() => {});
		return sent;
	}

	/**
	 * This side is leaving the session: finish the stream once everything sent is written, and
	 * wait for the peer to take it. A stream that never opened stays unopened.
	 */
	finish(): Promise<void> {
		this.#finished = true;
		const opened = this.#writer;
		const done = this.#tail.then(async () => {
			if (!opened) return;
			const writer = await opened;
			writer.close();
			await writer.closed;
		});
		this.#tail = done.catch(() => {});
		return done;
	}
}
