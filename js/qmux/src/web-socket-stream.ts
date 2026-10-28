/** A ponyfill for the WHATWG [`WebSocketStream`][spec] API.
 *
 * `WebSocketStream` exposes a WebSocket as a pair of streams —
 * `{ readable, writable }` — with backpressure on the writable. It currently
 * ships only in Chromium. This module wraps a plain `WebSocket` to present the
 * same surface (`opened`, `closed`, `close()`), applying write backpressure by
 * polling `bufferedAmount` against a configurable high-water mark.
 *
 * Note: a ponyfill can only *approximate* backpressure via `bufferedAmount`;
 * the native API observes the real send buffer. When the native API is present,
 * prefer it via {@link openWebSocketStream}.
 *
 * [spec]: https://github.com/ricea/websocketstream-explainer
 */

import { Milli, Nano, sleep } from "@moq/clock";

/** The data yielded by the readable / accepted by the writable. */
export type WebSocketStreamData = Uint8Array | string;

/** Resolved value of {@link WebSocketStreamLike.opened}. */
export interface WebSocketStreamOpenEvent {
	/** The messages the peer sends. */
	readable: ReadableStream<WebSocketStreamData>;
	/** The messages to send to the peer. */
	writable: WritableStream<WebSocketStreamData>;
	/** The extensions the server selected. */
	extensions: string;
	/** The subprotocol the server selected, or empty. */
	protocol: string;
}

/** Resolved value of {@link WebSocketStreamLike.closed}. */
export interface WebSocketStreamCloseEvent {
	/** The close code the connection ended with. */
	closeCode: number;
	/** The close reason the connection ended with. */
	reason: string;
}

/** Argument to {@link WebSocketStreamLike.close}. */
export interface WebSocketStreamCloseInfo {
	/** The close code to send. */
	closeCode?: number;
	/** The close reason to send. */
	reason?: string;
}

/** The common shape implemented by both the native API and this ponyfill. */
export interface WebSocketStreamLike {
	/** The URL the socket connected to. */
	readonly url: string;
	/** Resolves with the streams once the handshake completes. */
	readonly opened: Promise<WebSocketStreamOpenEvent>;
	/** Resolves once the connection closes. */
	readonly closed: Promise<WebSocketStreamCloseEvent>;
	/** Close the connection, with an optional code and reason. */
	close(closeInfo?: WebSocketStreamCloseInfo): void;
	/** Ponyfill-only: resize the write-backpressure high-water mark at runtime.
	 *  Absent on the native API, which sizes its own send buffer — callers should treat it as optional. */
	setHighWaterMark?(bytes: number): void;
}

/** Chromium's native API yields binary messages as `ArrayBuffer`. The public
 *  `WebSocketStreamLike` shape exposes normalized `Uint8Array` chunks so callers
 *  see the same data type whether the native implementation or ponyfill wins. */
type NativeWebSocketStreamData = WebSocketStreamData | ArrayBuffer;

interface NativeWebSocketStreamOpenEvent {
	readable: ReadableStream<NativeWebSocketStreamData>;
	writable: WritableStream<WebSocketStreamData>;
	extensions: string;
	protocol: string;
}

interface NativeWebSocketStreamLike {
	readonly url: string;
	readonly opened: Promise<NativeWebSocketStreamOpenEvent>;
	readonly closed: Promise<WebSocketStreamCloseEvent>;
	close(closeInfo?: WebSocketStreamCloseInfo): void;
}

type NativeWebSocketStreamConstructor = new (
	url: string,
	options?: Pick<WebSocketStreamOptions, "protocols" | "signal">,
) => NativeWebSocketStreamLike;

/** The slice of the `WebSocket` API this ponyfill relies on. Both the browser
 *  `WebSocket` and Node's `ws` satisfy it. */
export interface WebSocketLike {
	/** How binary messages arrive; set to `arraybuffer`. */
	binaryType: string;
	/** Absent on some server-side implementations; only used to report
	 *  {@link WebSocketStreamLike.url} for an adopted socket. */
	readonly url?: string;
	/** Bytes queued to send but not yet sent. */
	readonly bufferedAmount: number;
	/** The connection state, as the `WebSocket` constants number it. */
	readonly readyState: number;
	/** The subprotocol the server selected, or empty. */
	readonly protocol: string;
	/** The extensions the server selected. */
	readonly extensions: string;
	/** Queue a message to send. */
	send(data: string | ArrayBufferLike | ArrayBufferView): void;
	/** Close the connection, with an optional code and reason. */
	close(code?: number, reason?: string): void;
	/** Called once the handshake completes. */
	onopen: ((ev: unknown) => void) | null;
	/** Called with each message received. */
	onmessage: ((ev: { data: unknown }) => void) | null;
	/** Called when the connection fails. */
	onerror: ((ev: unknown) => void) | null;
	/** Called once the connection closes. */
	onclose: ((ev: { code?: number; reason?: string }) => void) | null;
}

/** Constructor for a {@link WebSocketLike} (e.g. the global `WebSocket` or `ws`). */
export type WebSocketConstructor = new (url: string, protocols?: string | string[]) => WebSocketLike;

export interface WebSocketStreamOptions {
	/** Subprotocols to advertise via `Sec-WebSocket-Protocol`. */
	protocols?: string[];
	/** Abort the connection. */
	signal?: AbortSignal;
	/** Ponyfill-only: the `WebSocket` implementation to use. Defaults to
	 *  `globalThis.WebSocket`. Pass Node's `ws` when there is no global. */
	webSocket?: WebSocketConstructor;
	/** Ponyfill-only: write backpressure kicks in once `bufferedAmount` exceeds
	 *  this many bytes. Defaults to 64 KiB. */
	highWaterMark?: number;
}

const CONNECTING = 0;
const OPEN = 1;
const DEFAULT_HIGH_WATER_MARK = 64 * 1024;

/** WebSocket close codes outside 1000 / 3000–4999 are rejected by `close()`. */
function validCloseCode(code: number | undefined): code is number {
	return code !== undefined && (code === 1000 || (code >= 3000 && code <= 4999));
}

/** Resolve once `bufferedAmount` drains below the mark (write backpressure).
 *  The mark is read live via `highWaterMark()` on every check, so a runtime
 *  resize (see {@link WebSocketStream.setHighWaterMark}) takes effect mid-drain.
 *  Returns `undefined` synchronously when there's already room — no microtask
 *  on the hot path, so `writer.ready` stays resolved when the socket is keeping up. */
function drain(ws: WebSocketLike, highWaterMark: () => number): Promise<void> | undefined {
	if (ws.bufferedAmount <= highWaterMark()) return undefined;
	return (async () => {
		while (ws.bufferedAmount > highWaterMark()) {
			// The socket closed mid-write: reject so the failure propagates rather
			// than resolving the write as if it had succeeded.
			if (ws.readyState > OPEN) throw new Error("WebSocket is closing");
			await sleep(Nano.fromMilli(Milli(10)));
		}
	})();
}

/** A `WebSocketStream` implemented over a plain `WebSocket`. */
export class WebSocketStream implements WebSocketStreamLike {
	readonly url: string;
	readonly opened: Promise<WebSocketStreamOpenEvent>;
	readonly closed: Promise<WebSocketStreamCloseEvent>;
	#ws: WebSocketLike;
	#highWaterMark: number;

	/** Dial `url`, or wrap a socket you already have (see {@link WebSocketStream.adopt}). */
	constructor(source: string | WebSocketLike, options: WebSocketStreamOptions = {}) {
		let ws: WebSocketLike;
		if (typeof source === "string") {
			const Ctor = options.webSocket ?? (globalThis as { WebSocket?: WebSocketConstructor }).WebSocket;
			if (!Ctor) {
				throw new Error("No WebSocket implementation found; pass options.webSocket");
			}
			ws = new Ctor(source, options.protocols);
			this.url = source;
		} else {
			ws = source;
			this.url = source.url ?? "";
		}
		ws.binaryType = "arraybuffer";
		this.#ws = ws;
		this.#highWaterMark = Math.max(1, Math.floor(options.highWaterMark ?? DEFAULT_HIGH_WATER_MARK));

		const opened = Promise.withResolvers<WebSocketStreamOpenEvent>();
		const closed = Promise.withResolvers<WebSocketStreamCloseEvent>();
		this.opened = opened.promise;
		this.closed = closed.promise;
		// Avoid unhandled-rejection noise if a caller only awaits `closed`.
		this.opened.catch(() => {});

		let controller: ReadableStreamDefaultController<WebSocketStreamData> | undefined;
		const readable = new ReadableStream<WebSocketStreamData>({
			start: (c) => {
				controller = c;
			},
			cancel: () => ws.close(),
		});

		const writable = new WritableStream<WebSocketStreamData>({
			write: (chunk) => {
				ws.send(chunk);
				return drain(ws, () => this.#highWaterMark);
			},
			// A WebSocket has no half-close, so closing/aborting the writable closes
			// the whole socket. Without this, `writer.close()` would leave it open.
			close: () => ws.close(),
			abort: () => ws.close(),
		});

		ws.onopen = () => {
			opened.resolve({ readable, writable, extensions: ws.extensions, protocol: ws.protocol });
		};
		ws.onmessage = (event) => {
			const data = event.data;
			if (typeof data === "string") {
				controller?.enqueue(data);
			} else if (data instanceof ArrayBuffer) {
				controller?.enqueue(new Uint8Array(data));
			} else if (ArrayBuffer.isView(data)) {
				const view = data as ArrayBufferView;
				controller?.enqueue(new Uint8Array(view.buffer, view.byteOffset, view.byteLength));
			}
		};
		ws.onerror = () => {
			const err = new Error("WebSocket connection error");
			opened.reject(err);
			try {
				controller?.error(err);
			} catch {}
		};
		ws.onclose = (event) => {
			opened.reject(new Error("WebSocket closed before opening"));
			try {
				controller?.close();
			} catch {}
			closed.resolve({ closeCode: event.code ?? 1006, reason: event.reason ?? "" });
		};

		// An adopted socket may already have finished (or missed) its handshake, in
		// which case `onopen`/`onclose` will never fire — settle from readyState.
		if (ws.readyState !== CONNECTING) {
			if (ws.readyState === OPEN) {
				opened.resolve({ readable, writable, extensions: ws.extensions, protocol: ws.protocol });
			} else {
				opened.reject(new Error("WebSocket is already closed"));
				try {
					controller?.close();
				} catch {}
				closed.resolve({ closeCode: 1006, reason: "" });
			}
		}

		const { signal } = options;
		if (signal) {
			if (signal.aborted) ws.close();
			else signal.addEventListener("abort", () => ws.close(), { once: true });
		}
	}

	/** Wrap a socket that already exists — typically one a server accepted from an
	 *  HTTP upgrade (`Deno.upgradeWebSocket`, `ws`, ...), where there is no URL to
	 *  dial and the handshake (including subprotocol selection) is already done.
	 *
	 *  Ownership transfers: this overwrites the socket's `onopen`/`onmessage`/
	 *  `onerror`/`onclose` handlers, so adopt it before anything else reads from
	 *  it — messages delivered before adoption are dropped by the platform, not
	 *  buffered. A socket that is already open resolves {@link opened} without
	 *  waiting for an `onopen` that has already fired (or was never going to).
	 *
	 *  `options.protocols` and `options.webSocket` are ignored; the socket exists. */
	static adopt(ws: WebSocketLike, options: Pick<WebSocketStreamOptions, "highWaterMark" | "signal"> = {}) {
		return new WebSocketStream(ws, options);
	}

	close(closeInfo: WebSocketStreamCloseInfo = {}): void {
		if (validCloseCode(closeInfo.closeCode)) {
			this.#ws.close(closeInfo.closeCode, closeInfo.reason);
		} else {
			// A reason can't be sent without a valid code, so drop both.
			this.#ws.close();
		}
	}

	/** The current write-backpressure high-water mark, in bytes. */
	get highWaterMark(): number {
		return this.#highWaterMark;
	}

	/** Resize the write-backpressure high-water mark at runtime (bytes).
	 *
	 * Set this to roughly the bandwidth-delay product (RTT × estimated
	 * throughput): large enough to keep the socket busy, small enough that
	 * queued bytes can still be reprioritized rather than committed to the OS
	 * send buffer. Takes effect immediately, including for an in-progress drain.
	 * Clamped to a minimum of 1 byte. */
	setHighWaterMark(bytes: number): void {
		this.#highWaterMark = Math.max(1, Math.floor(bytes));
	}
}

/** Open a `WebSocketStream`, preferring the native API when available and
 *  falling back to the {@link WebSocketStream} ponyfill otherwise. Passing
 *  `options.webSocket` forces the ponyfill (the native API can't use an injected
 *  socket). */
export function openWebSocketStream(url: string | URL, options: WebSocketStreamOptions = {}): WebSocketStreamLike {
	const href = typeof url === "string" ? url : url.toString();
	const Native = (globalThis as { WebSocketStream?: NativeWebSocketStreamConstructor }).WebSocketStream;
	if (Native && !options.webSocket) {
		const native = new Native(href, { protocols: options.protocols, signal: options.signal });
		return {
			url: native.url,
			opened: native.opened.then((event) => ({
				...event,
				readable: event.readable.pipeThrough(
					new TransformStream<NativeWebSocketStreamData, WebSocketStreamData>({
						transform(chunk, controller) {
							controller.enqueue(chunk instanceof ArrayBuffer ? new Uint8Array(chunk) : chunk);
						},
					}),
				),
			})),
			closed: native.closed,
			close: (closeInfo) => native.close(closeInfo),
		};
	}
	return new WebSocketStream(href, options);
}
