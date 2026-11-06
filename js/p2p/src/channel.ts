const RECORD_SIZE = 16 * 1024;
// Qmux-01 does not permit a smaller advertised record limit.
const MIN_RECORD_SIZE = 16382;
const SEND_WINDOW = RECORD_SIZE * 4;

/** The data channel a {@link Channel} owns, and whether it arrived from the peer. */
export interface ChannelProps {
	/** The peer connection carrying the channel, read for its SCTP message limit. */
	peer: RTCPeerConnection;
	/** The reliable ordered data channel to own. */
	channel: RTCDataChannel;
	/** The negotiated MoQ ALPN. */
	protocol: string;
	/** Whether the peer created the channel, so this side writes only after it hears the peer. */
	accepted: boolean;
}

/** The WebSocketStream shape qmux consumes, backed by one owned data channel. */
export class Channel {
	readonly url = "webrtc://data-channel";
	readonly opened;
	readonly closed;
	maxRecordSize = RECORD_SIZE;
	#open = Promise.withResolvers<{
		readable: ReadableStream<Uint8Array | string>;
		writable: WritableStream<Uint8Array | string>;
		extensions: string;
		protocol: string;
	}>();
	#closed = Promise.withResolvers<{ closeCode: number; reason: string }>();
	#drain?: ReturnType<typeof Promise.withResolvers<void>>;
	#reader?: ReadableStreamDefaultController<Uint8Array>;
	#ended = false;
	// Pending until an accepted channel hears its dialer or starts closing, then cleared so writes skip it.
	#unheard?: ReturnType<typeof Promise.withResolvers<void>>;
	#readable: ReadableStream<Uint8Array>;
	#writable: WritableStream<Uint8Array | string>;

	private peer: RTCPeerConnection;
	private channel: RTCDataChannel;
	private protocol: string;

	constructor({ peer, channel, protocol, accepted }: ChannelProps) {
		this.peer = peer;
		this.channel = channel;
		this.protocol = protocol;
		// Chromium reports a channel the peer announced as open before its SCTP stream is, and
		// silently drops what is sent in between. Only a delivered record proves the stream open,
		// and the dialing side always speaks first, so the accepting side holds its writes until it
		// hears the dialer. Only writes wait: the session, and its idle deadline, start on open.
		if (accepted) {
			this.#unheard = Promise.withResolvers<void>();
			void this.#unheard.promise.catch(() => {});
		}
		this.opened = this.#open.promise;
		this.closed = this.#closed.promise;
		void this.opened.catch(() => {});
		void this.closed.catch(() => {});
		channel.binaryType = "arraybuffer";
		channel.bufferedAmountLowThreshold = SEND_WINDOW / 2;
		this.#readable = new ReadableStream<Uint8Array>(
			{
				start: (controller) => {
					this.#reader = controller;
				},
				cancel: () => {
					this.#reader = undefined;
					this.close();
				},
			},
			new ByteLengthQueuingStrategy({ highWaterMark: SEND_WINDOW }),
		);
		// An in-flight write retains the queue slot until the browser's send buffer
		// falls below its low threshold, so writer.ready follows actual backpressure.
		this.#writable = new WritableStream(
			{
				write: (data) =>
					this.#write(data).catch((error) => {
						this.#finish(error);
						throw error;
					}),
				close: () => this.close(),
				abort: (reason) => this.#finish(reason instanceof Error ? reason : new Error(String(reason))),
			},
			{ highWaterMark: 1 },
		);
		channel.addEventListener("open", this.#onOpen);
		channel.addEventListener("message", this.#onMessage);
		channel.addEventListener("bufferedamountlow", this.#onDrain);
		channel.addEventListener("close", this.#onClose);
		channel.addEventListener("error", this.#onError);
		if (channel.readyState === "open") this.#onOpen();
		else if (channel.readyState !== "connecting") this.#finish(new Error("data channel is already closing"));
	}

	#onOpen = () => {
		if (this.#ended) return;
		// WebRTC reports an unbounded negotiated message size as positive Infinity.
		const max = this.peer.sctp?.maxMessageSize;
		if (max === undefined || Number.isNaN(max) || max < 0) {
			this.#finish(new Error("data channel has no negotiated SCTP message limit"));
			return;
		}
		this.maxRecordSize = Math.min(RECORD_SIZE, max);
		if (this.maxRecordSize < MIN_RECORD_SIZE) {
			this.#finish(new Error(`SCTP message limit ${max} is smaller than qmux's minimum ${MIN_RECORD_SIZE}`));
			return;
		}
		this.#open.resolve({
			readable: this.#readable,
			writable: this.#writable,
			extensions: "",
			protocol: `qmux-01.${this.protocol}`,
		});
	};

	#onMessage = (event: MessageEvent) => {
		if (this.#ended) return;
		if (!(event.data instanceof ArrayBuffer)) {
			this.#finish(new Error("qmux requires binary data-channel messages"));
			return;
		}
		if (event.data.byteLength > this.maxRecordSize || (this.#reader?.desiredSize ?? 0) < event.data.byteLength) {
			this.#finish(new Error("data-channel receive record exceeds its bounded queue"));
			return;
		}
		this.#reader?.enqueue(new Uint8Array(event.data));
		this.release();
	};
	#onDrain = () => {
		this.#drain?.resolve();
		this.#drain = undefined;
	};
	#onClose = () => this.#finish();
	#onError = (event: Event) =>
		this.#finish("error" in event && event.error instanceof Error ? event.error : new Error("data channel failed"));

	async #wait() {
		if (this.#ended || this.channel.readyState !== "open") throw new Error("data channel is closed");
		if (this.channel.bufferedAmount <= this.channel.bufferedAmountLowThreshold) return;
		this.#drain ??= Promise.withResolvers<void>();
		await this.#drain.promise;
	}

	async #write(data: Uint8Array | string) {
		await this.opened;
		if (typeof data === "string") throw new TypeError("qmux requires binary records");
		if (data.byteLength > this.maxRecordSize) throw new RangeError("qmux record exceeds the SCTP message limit");
		if (this.#unheard) await this.#unheard.promise;
		await this.#wait();
		this.channel.send(data.buffer instanceof ArrayBuffer ? (data as Uint8Array<ArrayBuffer>) : data.slice());
		await this.#wait();
	}

	#finish(error?: Error) {
		if (this.#ended) return;
		this.#ended = true;
		const closed = error ?? new Error("data channel is closed");
		this.#open.reject(closed);
		this.#unheard?.reject(closed);
		this.#unheard = undefined;
		this.#drain?.reject(closed);
		this.#drain = undefined;
		if (error) {
			this.#reader?.error(error);
			this.#closed.reject(error);
		} else {
			this.#reader?.close();
			this.#closed.resolve({ closeCode: 1000, reason: "" });
		}
		this.#reader = undefined;
		this.channel.removeEventListener("open", this.#onOpen);
		this.channel.removeEventListener("message", this.#onMessage);
		this.channel.removeEventListener("bufferedamountlow", this.#onDrain);
		this.channel.removeEventListener("close", this.#onClose);
		this.channel.removeEventListener("error", this.#onError);
		this.channel.close();
	}

	/** Stop holding writes for the dialer, so a closing session can flush its close frame. */
	release() {
		// A send Chromium drops now loses nothing: the channel closes once the flush settles.
		this.#unheard?.resolve();
		this.#unheard = undefined;
	}

	close() {
		this.#finish();
	}
}
