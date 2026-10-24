import Session from "@moq/qmux";
import { Channel } from "./channel.ts";

/** The peer connection and the MoQ ALPN agreed through signaling. */
export interface ConnectProps {
	/** The caller-owned peer connection. */
	peer: RTCPeerConnection;
	/** The negotiated MoQ ALPN, such as moq-lite-07-wip. */
	protocol: string;
}

/** An incoming data channel and its agreed MoQ protocol. */
export interface AcceptProps extends ConnectProps {
	/** The reliable ordered data channel whose ownership transfers to the transport. */
	channel: RTCDataChannel;
}

function incoming<T>(stream: Promise<ReadableStream<T>>): ReadableStream<T> {
	const opened = stream.then((stream) => stream.getReader());
	return new ReadableStream({
		async start() {
			await opened;
		},
		async pull(controller) {
			const reader = await opened;
			const { value, done } = await reader.read();
			if (done) {
				reader.releaseLock();
				controller.close();
			} else controller.enqueue(value);
		},
		async cancel(reason) {
			const reader = await opened;
			await reader.cancel(reason);
			reader.releaseLock();
		},
	});
}

/** A WebTransport-shaped handle owning one reliable ordered data channel. */
export class Transport implements WebTransport {
	/** The transport label used by MoQ connection statistics. */
	readonly transport = "webrtc";
	/** The MoQ ALPN agreed through signaling. */
	readonly protocol: string;
	/** Resolves once the channel and qmux session are ready. */
	readonly ready: Promise<void>;
	/** Resolves on an application close or rejects on transport failure. */
	readonly closed: Promise<WebTransportCloseInfo>;
	/** The connection uses reliable delivery only. */
	readonly reliability = "reliable-only";
	/** SCTP controls congestion for the connection. */
	readonly congestionControl = "default";
	/** Incoming bidirectional qmux streams. */
	readonly incomingBidirectionalStreams: ReadableStream<WebTransportBidirectionalStream>;
	/** Incoming unidirectional qmux streams. */
	readonly incomingUnidirectionalStreams: ReadableStream<ReadableStream<Uint8Array>>;
	/** Datagram-shaped streams with a zero send limit, since this binding has no datagrams. */
	readonly datagrams: WebTransportDatagramDuplexStream;
	#session: Promise<Session>;
	#active?: Session;
	#socket: Channel;
	#closing = false;
	#closed = Promise.withResolvers<WebTransportCloseInfo>();

	private constructor({ peer, channel, protocol }: AcceptProps, server: boolean) {
		if (!protocol || /\s/.test(protocol)) throw new TypeError("a negotiated MoQ ALPN is required");
		if (!channel.ordered || channel.maxRetransmits !== null || channel.maxPacketLifeTime !== null) {
			throw new TypeError("qmux requires a reliable ordered data channel");
		}
		if (channel.protocol !== protocol) throw new TypeError("data-channel protocol does not match the agreed ALPN");
		this.protocol = protocol;
		this.#socket = new Channel(peer, channel, protocol);
		this.closed = this.#closed.promise;
		// SCTP's negotiated limit is known only after the data channel opens.
		this.#session = this.#socket.opened.then(() => {
			if (this.#closing) throw new Error("transport closed before opening");
			const config = { maxRecordSize: BigInt(this.#socket.maxRecordSize), maxDatagramFrameSize: null };
			const session = server
				? Session.accept(this.#socket, { config, protocol: `qmux-01.${protocol}` })
				: new Session(this.#socket, { config });
			this.#active = session;
			void session.closed.then(this.#closed.resolve, this.#closed.reject);
			return session;
		});
		this.ready = this.#session.then((session) => session.ready);
		void this.ready.catch((error) => {
			this.#closed.reject(error);
			this.#socket.close();
		});
		void this.closed.catch(() => {});
		this.incomingBidirectionalStreams = incoming(
			this.#session.then((session) => session.incomingBidirectionalStreams),
		);
		this.incomingUnidirectionalStreams = incoming(
			this.#session.then((session) => session.incomingUnidirectionalStreams),
		);
		let writer: WritableStreamDefaultWriter<Uint8Array>;
		this.datagrams = {
			maxDatagramSize: 0,
			incomingHighWaterMark: 1,
			incomingMaxAge: null,
			outgoingHighWaterMark: 1,
			outgoingMaxAge: null,
			readable: incoming(this.#session.then((session) => session.datagrams.readable)),
			writable: new WritableStream({
				start: async () => {
					writer = (await this.#session).datagrams.writable.getWriter();
				},
				write: (data) => writer.write(data),
				close: () => writer.close(),
				abort: (reason) => writer.abort(reason),
			}),
		};
	}

	/** Create a reliable ordered data channel, leaving the peer connection with its caller. */
	static connect({ peer, protocol }: ConnectProps): Transport {
		if (!protocol || /\s/.test(protocol)) throw new TypeError("a negotiated MoQ ALPN is required");
		const channel = peer.createDataChannel("moq", { ordered: true, protocol });
		try {
			return new Transport({ peer, channel, protocol }, false);
		} catch (error) {
			channel.close();
			throw error;
		}
	}

	/** Adopt an incoming reliable ordered data channel, leaving the peer connection with its caller. */
	static accept(props: AcceptProps): Transport {
		return new Transport(props, true);
	}

	/** Open a bidirectional qmux stream. */
	async createBidirectionalStream(options?: WebTransportSendStreamOptions): Promise<WebTransportBidirectionalStream> {
		return (await this.#session).createBidirectionalStream(options);
	}

	/** Open a unidirectional qmux stream. */
	async createUnidirectionalStream(options?: WebTransportSendStreamOptions): Promise<WritableStream<Uint8Array>> {
		return (await this.#session).createUnidirectionalStream(options);
	}

	/** Close the qmux session and its data channel. */
	close(info: WebTransportCloseInfo = {}) {
		if (this.#closing) return;
		if (this.#active) {
			this.#active.close(info);
			this.#closing = true;
		} else {
			this.#closing = true;
			this.#closed.resolve(info);
			this.#socket.close();
		}
	}

	/** Refuse HTTP/3 statistics, which this data-channel binding does not provide. */
	async getStats(): Promise<never> {
		throw new Error("WebTransport connection statistics are unavailable over WebRTC");
	}
}
