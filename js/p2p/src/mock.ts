class MockChannel extends EventTarget {
	binaryType = "blob";
	bufferedAmount = 0;
	bufferedAmountLowThreshold = 0;
	readyState: RTCDataChannelState = "connecting";
	ordered = true;
	maxRetransmits: number | null = null;
	maxPacketLifeTime: number | null = null;
	protocol: string;
	partner!: MockChannel;
	autoDrain = true;
	records: Uint8Array[] = [];
	listeners = new Set<EventListenerOrEventListenerObject>();
	constructor(protocol: string) {
		super();
		this.protocol = protocol;
	}
	override addEventListener(
		type: string,
		callback: EventListenerOrEventListenerObject | null,
		options?: AddEventListenerOptions | boolean,
	) {
		if (callback) this.listeners.add(callback);
		super.addEventListener(type, callback, options);
	}
	override removeEventListener(
		type: string,
		callback: EventListenerOrEventListenerObject | null,
		options?: EventListenerOptions | boolean,
	) {
		if (callback) this.listeners.delete(callback);
		super.removeEventListener(type, callback, options);
	}
	open() {
		this.readyState = "open";
		this.dispatchEvent(new Event("open"));
	}
	send(data: Uint8Array) {
		if (this.readyState !== "open") throw new Error("send on closed channel");
		const record = data.slice();
		this.records.push(record);
		this.bufferedAmount += record.byteLength;
		queueMicrotask(() => {
			if (this.partner.readyState === "open")
				this.partner.dispatchEvent(new MessageEvent("message", { data: record.buffer }));
			if (this.autoDrain) this.drain();
		});
	}
	drain() {
		this.bufferedAmount = 0;
		this.dispatchEvent(new Event("bufferedamountlow"));
	}
	close() {
		if (this.readyState === "closed") return;
		this.readyState = "closed";
		queueMicrotask(() => {
			this.dispatchEvent(new Event("close"));
			if (this.partner.readyState !== "closed") {
				this.partner.readyState = "closed";
				this.partner.dispatchEvent(new Event("close"));
			}
		});
	}
	asChannel(): RTCDataChannel {
		return this as unknown as RTCDataChannel;
	}
}

class MockPeer {
	sctp = { maxMessageSize: 65536 };
	#channel?: MockChannel;
	#incoming?: MockChannel;
	partner!: MockPeer;
	closed = false;
	options?: RTCDataChannelInit;
	createDataChannel(_label: string, options: RTCDataChannelInit) {
		this.options = options;
		const channel = new MockChannel(options.protocol ?? "");
		const incoming = new MockChannel(options.protocol ?? "");
		channel.partner = incoming;
		incoming.partner = channel;
		this.#channel = channel;
		this.partner.#incoming = incoming;
		return channel.asChannel();
	}
	get channel() {
		if (!this.#channel) throw new Error("mock peer has no outgoing channel");
		return this.#channel;
	}
	get incoming() {
		if (!this.#incoming) throw new Error("mock peer has no incoming channel");
		return this.#incoming;
	}

	close() {
		this.closed = true;
	}
	asPeer(): RTCPeerConnection {
		return this as unknown as RTCPeerConnection;
	}
}

export function peers(maxMessageSize = 65536) {
	const left = new MockPeer();
	const right = new MockPeer();
	left.partner = right;
	right.partner = left;
	left.sctp.maxMessageSize = right.sctp.maxMessageSize = maxMessageSize;
	return { left, right };
}
