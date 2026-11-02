import { Instant, type Interval, interval, Milli, MissedTickBehavior, Nano, now, Second, timeout } from "@moq/clock";
import { Credit, replenishWindow } from "./credit.ts";
import { resetCode, SessionError, StreamError, streamCode } from "./error.ts";
import type { TransportParams, WireFormat } from "./frame.ts";
import * as Frame from "./frame.ts";
import { DEFAULT_TRANSPORT_PARAMS, usesRecords } from "./frame.ts";
import { RecvStream } from "./recv.ts";
import { RecvOpen } from "./recv-open.ts";
import { DEFAULT_SEND_ORDER, SendScheduler, type SendSink, WritableStreamSink } from "./scheduler.ts";
import * as Stream from "./stream.ts";
import { VarInt } from "./varint.ts";
import {
	openWebSocketStream,
	type WebSocketLike,
	WebSocketStream,
	type WebSocketStreamLike,
} from "./web-socket-stream.ts";

/** The QMux wire-format versions a caller can advertise. @reexport */
export type Version = WireFormat;

/** Options for a new outgoing stream. `@types/web` doesn't declare `waitUntilAvailable` yet. */
export type SendStreamOptions = WebTransportSendStreamOptions & {
	/** Wait for the peer to grant stream credit (the default) instead of rejecting at once. */
	waitUntilAvailable?: boolean;
};

/** Configuration for a QMux session. */
export interface Config {
	/** Max concurrent bidirectional streams the peer can open. */
	maxStreamsBidi?: bigint;
	/** Max concurrent unidirectional streams the peer can open. */
	maxStreamsUni?: bigint;
	/** Connection-level receive window in bytes. */
	receiveWindow?: bigint;
	/** Receive window in bytes for every stream. */
	streamReceiveWindow?: bigint;
	/** Idle timeout in milliseconds, or null to disable it; zero is refused. */
	idleTimeout?: number | null;
	/** Maximum QMux Record size in bytes (draft-01). */
	maxRecordSize?: bigint;
	/** Largest DATAGRAM *frame* (RFC 9221: type + length + payload) we advertise
	 *  willingness to receive; null disables datagrams. This is a frame size, not a
	 *  payload size; `Session.datagrams.maxDatagramSize` reports the usable payload
	 *  (this value less framing overhead). Keep it at or below `maxRecordSize`.
	 *  Datagrams are a record-framed-draft feature (qmux-01+); this is ignored on
	 *  qmux-00. Defaults to a full record. */
	maxDatagramFrameSize?: bigint | null;
}

const DEFAULT_CONFIG: Required<Config> = {
	maxStreamsBidi: 100n,
	maxStreamsUni: 100n,
	receiveWindow: 1_048_576n,
	streamReceiveWindow: 262_144n,
	idleTimeout: 30_000,
	maxRecordSize: Frame.DEFAULT_MAX_RECORD_SIZE,
	// Fill a full record by default; the record layer bounds the size.
	maxDatagramFrameSize: Frame.DEFAULT_MAX_RECORD_SIZE,
};

function resolveConfig(config?: Config): Required<Config> {
	if (config !== undefined && (config === null || typeof config !== "object" || Array.isArray(config))) {
		throw new TypeError("config must be an object");
	}
	const resolved = { ...DEFAULT_CONFIG };
	for (const key of Reflect.ownKeys(config ?? {})) {
		if (typeof key !== "string" || !Object.hasOwn(DEFAULT_CONFIG, key)) {
			throw new TypeError(`Unsupported QMux config setting: ${String(key)}`);
		}
		const value = config?.[key as keyof Config];
		if (value !== undefined) Object.assign(resolved, { [key]: value });
	}
	for (const [key, value] of Object.entries(resolved)) {
		if (key === "idleTimeout") {
			if (value !== null && (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0)) {
				throw new TypeError("idleTimeout must be a positive safe integer or null");
			}
		} else if (key !== "maxDatagramFrameSize" || value !== null) {
			if (typeof value !== "bigint") throw new TypeError(`${key} must be a bigint`);
			VarInt.from(value);
		}
	}
	if (resolved.maxDatagramFrameSize === 0n) throw new Error("maxDatagramFrameSize must be positive or null");
	if (resolved.maxRecordSize < Frame.DEFAULT_MAX_RECORD_SIZE) {
		throw new Error(`maxRecordSize must be at least ${Frame.DEFAULT_MAX_RECORD_SIZE}`);
	}
	return resolved;
}

function configToTransportParams(config: Required<Config>): TransportParams {
	return {
		maxIdleTimeout: BigInt(config.idleTimeout ?? 0),
		initialMaxData: config.receiveWindow,
		initialMaxStreamDataBidiLocal: config.streamReceiveWindow,
		initialMaxStreamDataBidiRemote: config.streamReceiveWindow,
		initialMaxStreamDataUni: config.streamReceiveWindow,
		initialMaxStreamsBidi: config.maxStreamsBidi,
		initialMaxStreamsUni: config.maxStreamsUni,
		// Clamp to maxRecordSize so we never advertise a datagram larger than our
		// record layer accepts.
		maxDatagramFrameSize:
			config.maxDatagramFrameSize === null
				? 0n
				: // biome-ignore lint/complexity/useMathMinMax: bigint operands; Math.min/max throw on bigint.
					config.maxDatagramFrameSize < config.maxRecordSize
					? config.maxDatagramFrameSize
					: config.maxRecordSize,
		maxRecordSize: config.maxRecordSize,
		// Version-gated in #startSession (only advertised on qmux-02).
		resetStreamAt: false,
	};
}

// Only the session can construct or mutate the datagram transport.
const datagramsKey = Symbol("Datagrams");
let makeDatagrams: (send: (data: Uint8Array) => void) => Datagrams;
let pushDatagram: (datagrams: Datagrams, data: Uint8Array) => void;
let setMaxDatagramSize: (datagrams: Datagrams, size: number) => void;
let closeDatagrams: (datagrams: Datagrams, err?: Error) => void;

/** WebTransport datagrams with a live inbound queue limit and no expiry. */
class Datagrams implements WebTransportDatagramDuplexStream {
	/** The datagrams the peer sends. */
	readonly readable: ReadableStream<Uint8Array>;
	/** The datagrams to send to the peer. */
	readonly writable: WritableStream<Uint8Array>;

	#incomingHighWaterMark = 1024;
	#incoming!: ReadableStreamDefaultController<Uint8Array>;
	#pending: Uint8Array[] = [];
	#pullResolve?: () => void;
	#closed = false;
	// Resolved from the peer's transport parameters once the handshake completes;
	// 0 until then (and forever if the peer doesn't accept datagrams).
	#maxDatagramSize = 0;

	private constructor(key: typeof datagramsKey, send: (data: Uint8Array) => void) {
		if (key !== datagramsKey) throw new TypeError("Datagrams can only be constructed by Session");
		this.readable = new ReadableStream<Uint8Array>(
			{
				start: (controller) => {
					this.#incoming = controller;
				},
				pull: () => {
					const data = this.#pending.shift();
					if (data) this.#incoming.enqueue(data);
					if (this.#closed && this.#pending.length === 0) this.#incoming.close();
					if (data || this.#closed) return;
					return new Promise<void>((resolve) => {
						this.#pullResolve = resolve;
					});
				},
				cancel: () => {
					this.#closed = true;
					this.#pending = [];
					this.#resolvePull();
				},
			},
			// Keep the backlog here so the live policy counts only unread packets.
			{ highWaterMark: 0 },
		);

		this.writable = new WritableStream<Uint8Array>({
			write: (chunk) => {
				// Oversized or unsupported datagrams are discarded, not errored:
				// the writable stays usable, matching unreliable-datagram semantics.
				if (this.#maxDatagramSize > 0 && chunk.byteLength <= this.#maxDatagramSize) send(chunk);
			},
		});
	}

	static {
		makeDatagrams = (send) => new Datagrams(datagramsKey, send);
		pushDatagram = (datagrams, data) => datagrams.#push(data);
		setMaxDatagramSize = (datagrams, size) => {
			datagrams.#maxDatagramSize = size;
		};
		closeDatagrams = (datagrams, err) => datagrams.#close(err);
	}

	/** The largest payload a datagram can carry, or 0 before the peer allows any. */
	get maxDatagramSize(): number {
		return this.#maxDatagramSize;
	}

	/** The live inbound queue limit; lowering it preserves already queued datagrams. */
	get incomingHighWaterMark(): number {
		return this.#incomingHighWaterMark;
	}

	set incomingHighWaterMark(value: number) {
		if (typeof value !== "number") throw new TypeError("incomingHighWaterMark must be a number");
		if (Number.isNaN(value) || value < 0) throw new RangeError("incomingHighWaterMark must be nonnegative");
		this.#incomingHighWaterMark = Math.max(1, value);
	}

	/** Inbound expiry is unsupported. */
	get incomingMaxAge(): number | null {
		return null;
	}

	set incomingMaxAge(value: number | null) {
		if (value !== null) throw new DOMException("Datagram expiry is unsupported", "NotSupportedError");
	}

	/** The outbound queue limit cannot be changed. */
	get outgoingHighWaterMark(): number {
		return 1024;
	}

	set outgoingHighWaterMark(value: number) {
		if (value !== 1024) throw new DOMException("Outbound queue policy is unsupported", "NotSupportedError");
	}

	/** Outbound expiry is unsupported. */
	get outgoingMaxAge(): number | null {
		return null;
	}

	set outgoingMaxAge(value: number | null) {
		if (value !== null) throw new DOMException("Datagram expiry is unsupported", "NotSupportedError");
	}

	#push(data: Uint8Array): void {
		if (this.#closed) return;
		if (this.#pullResolve) {
			this.#incoming.enqueue(data);
			this.#resolvePull();
		} else if (this.#pending.length < this.#incomingHighWaterMark) {
			this.#pending.push(data);
		}
	}

	#resolvePull(): void {
		const resolve = this.#pullResolve;
		this.#pullResolve = undefined;
		resolve?.();
	}

	#close(err?: Error): void {
		if (this.#closed) return;
		this.#closed = true;
		if (err) {
			this.#pending = [];
			this.#incoming.error(err);
		} else if (this.#pending.length === 0) {
			this.#incoming.close();
		}
		this.#resolvePull();
	}
}

interface PendingIncomingStream<T> {
	value: T;
	onAccept: () => void;
	onDrop: (err: Error) => void;
}

/** Application-facing incoming-stream acceptor with no implicit prefetch.
 *
 * A normal ReadableStream controller will happily queue past its high-water
 * mark; `desiredSize` is only a backpressure signal. QMux cannot stop reading
 * its underlying byte stream, so keep new streams in this explicit queue and
 * transfer ownership only in response to an application read. The peer's
 * MAX_STREAMS credit waits on `onAccept`, which bounds this pending queue to the
 * stream count we advertised.
 */
class IncomingStreamQueue<T> {
	readonly readable: ReadableStream<T>;

	#controller!: ReadableStreamDefaultController<T>;
	#pending: PendingIncomingStream<T>[] = [];
	#pullResolve?: () => void;
	#cancelled = false;
	#closed = false;

	constructor() {
		this.readable = new ReadableStream<T>(
			{
				start: (controller) => {
					this.#controller = controller;
				},
				pull: () => {
					if (this.#deliver()) return;
					return new Promise<void>((resolve) => {
						this.#pullResolve = resolve;
					});
				},
				cancel: () => {
					this.#cancelled = true;
					this.#dropPending(new Error("incoming stream acceptor cancelled"));
					this.#resolvePull();
				},
			},
			{ highWaterMark: 0 },
		);
	}

	push(value: T, onAccept: () => void, onDrop: (err: Error) => void): boolean {
		if (this.#cancelled || this.#closed) return false;
		this.#pending.push({ value, onAccept, onDrop });
		if (this.#pullResolve) {
			this.#deliver();
			this.#resolvePull();
		}
		return true;
	}

	close(err?: Error): void {
		if (this.#closed || this.#cancelled) return;
		this.#closed = true;
		this.#dropPending(err ?? new Error("session closed"));
		try {
			if (err) this.#controller.error(err);
			else this.#controller.close();
		} catch {}
		this.#resolvePull();
	}

	#deliver(): boolean {
		const item = this.#pending.shift();
		if (!item) return false;
		try {
			this.#controller.enqueue(item.value);
			item.onAccept();
		} catch {
			item.onDrop(new Error("incoming stream acceptor cancelled"));
		}
		return true;
	}

	#dropPending(err: Error): void {
		const pending = this.#pending;
		this.#pending = [];
		for (const item of pending) item.onDrop(err);
	}

	#resolvePull(): void {
		const resolve = this.#pullResolve;
		this.#pullResolve = undefined;
		resolve?.();
	}
}

/** Options for opening a QMux Session over WebSocket. */
export interface SessionOptions extends WebTransportOptions {
	/** Application-level subprotocols to advertise via `Sec-WebSocket-Protocol`.
	 *
	 * Each entry is either:
	 *  - A bare ALPN (e.g. `"moq-lite-04"`). The polyfill resolves it via
	 *    {@link SessionOptions.versions} and emits the prefixed wire form
	 *    `{version}.{alpn}` (e.g. `"qmux-01.moq-lite-04"`).
	 *  - An explicit pair already prefixed with a QMux version
	 *    (e.g. `"qmux-00.moq-transport-18"`). Advertised as-is.
	 *
	 * Bare entries that have no matching `versions` entry throw at
	 * construction time.
	 */
	protocols?: string[];

	/** Maps each bare ALPN to the QMux wire-format version(s) it can ride on.
	 *
	 * Required for every bare entry in `protocols`; entries already in
	 * `{qmux-VV}.{alpn}` pair form bypass this map. Per-entry semantics:
	 *
	 *  - `Version`: advertise exactly `{version}.{alpn}`.
	 *  - `Version[]`: advertise one `{v}.{alpn}` per array entry in order.
	 *    Lets the server pick across multiple QMux drafts.
	 *  - `null`: advertise the ALPN under every QMux version this polyfill
	 *    knows about (currently qmux-02, qmux-01, then qmux-00). Useful when the
	 *    app doesn't care which draft and wants forward compatibility.
	 *
	 */
	versions?: Record<string, Version | Version[] | null>;

	/** Require the peer to negotiate one of the app protocols in `protocols`.
	 *
	 * When false (the default), bare QMux version ALPNs are also offered.
	 * Set true to require a version-prefixed application protocol.
	 */
	requireProtocol?: boolean;

	/** QMux flow control configuration. Only used for the QMux wire formats. */
	config?: Config;

	/** Initial send-buffer high-water mark in bytes, used for write
	 *  backpressure. Only takes effect when falling back to the
	 *  `WebSocketStream` ponyfill (no native `WebSocketStream`); the
	 *  native API sizes its own send buffer. Defaults to 64 KiB.
	 *
	 *  For best throughput *and* prioritization, set this to roughly the
	 *  bandwidth-delay product (RTT × estimated throughput) and adjust it at
	 *  runtime with {@link Session.setSendBufferSize}. */
	sendBufferSize?: number;
}

/** Options for accepting a QMux Session on an already-negotiated socket.
 *
 * The handshake is done by the time [[Session.accept]] is called, so the
 * subprotocol options ({@link SessionOptions.protocols}, `versions`,
 * `requireProtocol`) don't apply — see [[selectSubprotocol]], which consumes
 * them during the upgrade instead.
 */
export interface AcceptOptions extends Pick<SessionOptions, "config" | "sendBufferSize"> {
	/** The negotiated `Sec-WebSocket-Protocol` value, for hosts that don't expose
	 *  it on the socket.
	 *
	 *  [[Session.accept]] reads `socket.protocol` by default, which is where the
	 *  wire-format version comes from. Set this when the socket reports an empty
	 *  protocol despite one having been negotiated — otherwise the session refuses the socket. Pass the full wire value (e.g. `"qmux-02.moq-lite-04"`), not
	 *  the bare app protocol. */
	protocol?: string;
}

/** Default send-buffer high-water mark (bytes) for the WebSocketStream ponyfill. */
const DEFAULT_SEND_BUFFER_SIZE = 64 * 1024;

/** How long a closing session waits for a backed-up socket to take its close
 *  frame, matching Rust's `CLOSE_TIMEOUT`. A peer that stopped reading can't
 *  hold the socket open past it. */
const CLOSE_TIMEOUT = Nano.fromSecond(Second(3));

/** Present the caller's socket as a `WebSocketStreamLike`, whichever they passed.
 *
 * A `WebSocketStream` (native or ponyfill) already has the shape; a plain
 * `WebSocket` gets wrapped, which takes ownership of its event handlers. */
function toWebSocketStream(socket: WebSocketLike | WebSocketStreamLike, highWaterMark: number): WebSocketStreamLike {
	return "opened" in socket ? socket : WebSocketStream.adopt(socket, { highWaterMark });
}

/** Get the subprotocol prefix for a QMux wire-format version. */
function versionPrefix(version: Version): string {
	switch (version) {
		case "qmux-02":
			return "qmux-02.";
		case "qmux-01":
			return "qmux-01.";
		case "qmux-00":
			return "qmux-00.";
		default:
			throw new Error(`Unsupported QMux version: ${JSON.stringify(version)}`);
	}
}

/** QMux versions recognized as prefixed ALPNs, newest first. */
const QMUX_VERSIONS = ["qmux-02", "qmux-01", "qmux-00"] as const satisfies readonly Version[];

/** Convert `http(s)://` to `ws(s)://`. Pass-through for `ws(s)://` URLs. */
function toWebSocketUrl(url: string | URL): string {
	const u = typeof url === "string" ? new URL(url) : url;
	let scheme: string;
	switch (u.protocol) {
		case "https:":
		case "wss:":
			scheme = "wss:";
			break;
		case "http:":
		case "ws:":
			scheme = "ws:";
			break;
		default:
			throw new Error(`Unsupported protocol: ${u.protocol}`);
	}
	return `${scheme}//${u.host}${u.pathname}${u.search}`;
}

/** Bare version ALPNs appended unless `requireProtocol` is set. Newest first. */
const BARE_ALPNS = ["qmux-02", "qmux-01", "qmux-00"] as const;

/** Resolve a `protocols` + `versions` pair to the wire subprotocol list.
 *
 * Expansion rules per bare entry's `versions` value: single `Version`
 * emits one pair; an array emits one pair per element; `null` emits one
 * pair per [[QMUX_VERSIONS]] entry. Entries already in `{qmux-VV}.{alpn}`
 * pair form pass through. Unless `requireProtocol` is true, the bare version
 * ALPNs (`qmux-02`, `qmux-01`, `qmux-00`) are appended after the
 * prefixed pairs so a peer that doesn't pin an app protocol can still
 * negotiate a wire format.
 *
 * Throws if any bare entry has no matching `versions` mapping.
 */
export function resolveSubprotocols(
	protocols: readonly string[],
	versions: Readonly<Record<string, Version | Version[] | null>>,
	requireProtocol: boolean,
): string[] {
	const out: string[] = [];
	for (const entry of protocols) {
		const known = QMUX_VERSIONS.find((v) => entry.startsWith(versionPrefix(v)));
		if (known !== undefined) {
			out.push(entry);
			continue;
		}
		if (!(entry in versions)) {
			throw new Error(
				`Sec-WebSocket-Protocol entry ${JSON.stringify(entry)} has no qmux prefix and no versions mapping`,
			);
		}
		const value = versions[entry];
		const expanded: readonly Version[] = value === null ? QMUX_VERSIONS : Array.isArray(value) ? value : [value];
		for (const v of expanded) {
			out.push(`${versionPrefix(v)}${entry}`);
		}
	}
	if (!requireProtocol) {
		out.push(...BARE_ALPNS);
	}
	return out;
}

/** Choose the `Sec-WebSocket-Protocol` value to accept from a client's offer.
 *
 * The server role performs the WebSocket handshake itself (`Deno.upgradeWebSocket`,
 * `ws`, `Bun.serve`, ...), so it — not this library — picks the subprotocol. This
 * is the server-side mirror of [[resolveSubprotocols]]: it expands the same
 * `protocols` + `versions` pair into the wire forms this session supports, in
 * preference order, and returns the first one the client offered. Pass the result
 * to the host's upgrade call, then hand the accepted socket to [[Session.accept]],
 * which derives the wire-format version from it.
 *
 * `offered` accepts the raw `Sec-WebSocket-Protocol` header (comma-separated) or
 * an already-split list. Returns `undefined` when nothing matches, which the
 * caller should turn into a failed upgrade rather than accepting the socket:
 * completing the handshake with no supported subprotocol is refused.
 *
 * Preference is ours, not the client's — the first entry in `protocols` that the
 * client also offered wins, matching the Rust `qmux::ws::Server`.
 *
 * @example
 * ```ts
 * const protocol = selectSubprotocol(req.headers.get("sec-websocket-protocol"), {
 *   protocols: ["moq-transport-18"],
 *   versions: { "moq-transport-18": null },
 * });
 * if (!protocol) return new Response("no supported protocol", { status: 400 });
 * const { socket, response } = Deno.upgradeWebSocket(req, { protocol });
 * ```
 */
export function selectSubprotocol(
	offered: string | readonly string[] | null | undefined,
	options?: Pick<SessionOptions, "protocols" | "versions" | "requireProtocol">,
): string | undefined {
	const wanted = typeof offered === "string" ? offered.split(",") : (offered ?? []);
	const client = new Set(wanted.map((p) => p.trim()).filter((p) => p !== ""));
	if (client.size === 0) return undefined;

	const ours = resolveSubprotocols(
		options?.protocols ?? [],
		options?.versions ?? {},
		options?.requireProtocol ?? false,
	);
	return ours.find((p) => client.has(p));
}

/** Pick the QMux wire-format version from a negotiated subprotocol. */
function detectVersion(negotiated: string): WireFormat {
	for (const v of QMUX_VERSIONS) {
		if (negotiated === v || negotiated.startsWith(versionPrefix(v))) {
			return v;
		}
	}
	throw new Error(`Unsupported QMux subprotocol: ${JSON.stringify(negotiated)}`);
}

/** Strip the version prefix from a negotiated `Sec-WebSocket-Protocol` value.
 *
 * Returns the application protocol name, or `""` if only the bare version
 * ALPN was negotiated. Unsupported subprotocols are refused before parsing.
 */
function parseProtocol(raw: string, version: WireFormat): string {
	if (raw === version) return "";
	const prefix = versionPrefix(version);
	return raw.startsWith(prefix) ? raw.slice(prefix.length) : "";
}

/** Per-stream flow control state. */
interface StreamFlowState {
	sendCredit: Credit;
	/** Bytes that have actually reached the transport for this send half. */
	sendOffset: bigint;
	recvMax: bigint;
	recvOffset: bigint;
	recvConsumed: bigint;
	/** Runs once when the send half ends, for a peer stream whose credit waits on it. */
	onSendEnd?: () => void;
}

/** A `WebTransport` session carried over a WebSocket, for hosts without native WebTransport. */
export default class Session implements WebTransport {
	// The transport: a native `WebSocketStream` when the platform has one (real
	// backpressure), otherwise the `WebSocketStream` ponyfill over a plain
	// `WebSocket` (bufferedAmount-based backpressure). Either way, one API.
	#wss?: WebSocketStreamLike;
	#scheduler?: SendScheduler;
	#sendBufferSize: number;
	// The stream-id role: false dials (client), true accepts (server). Set by
	// [[Session.accept]]; every id we mint and every id we allow the peer to mint
	// keys off it.
	#isServer = false;
	// [[AcceptOptions.protocol]]: stands in for `socket.protocol` when the host
	// doesn't expose the negotiated subprotocol.
	#protocolOverride?: string;
	// `SessionOptions.requireProtocol` on a dial: the server must pick one of
	// our application protocols, not answer with none.
	#requireProtocol = false;
	#closed?: Error;
	#closeReason?: Error;
	// The APPLICATION_CLOSE or CONNECTION_CLOSE that #teardown sends, if any.
	#closeFrame?: Uint8Array;

	#sendStreams = new Map<bigint, WritableStreamDefaultController>();
	#recvStreams = new Map<bigint, RecvStream>();

	#nextUniStreamId = 0n;
	#nextBiStreamId = 0n;

	// Used only after the negotiated subprotocol has been validated.
	#version: WireFormat = "qmux-02";

	#protocol = "";
	/** The negotiated application-level subprotocol, or empty string if none.
	 *
	 * The prefix is stripped; this returns only the application protocol name.
	 */
	get protocol(): string {
		return this.#protocol;
	}

	/** Resolves once the WebSocket opens and the session can carry streams. */
	readonly ready: Promise<void>;
	#readyResolve: () => void;
	#readyReject: (err: Error) => void;
	/** Resolves with the close code and reason on a clean close, or rejects with a {@link SessionError}. */
	readonly closed: Promise<WebTransportCloseInfo>;
	#closedResolve: (info: WebTransportCloseInfo) => void;
	#closedReject: (err: Error) => void;

	/** Bidirectional streams the peer opens, in arrival order. */
	readonly incomingBidirectionalStreams: ReadableStream<WebTransportBidirectionalStream>;
	#incomingBidirectionalQueue: IncomingStreamQueue<WebTransportBidirectionalStream>;
	/** Unidirectional streams the peer opens, in arrival order. */
	readonly incomingUnidirectionalStreams: ReadableStream<ReadableStream<Uint8Array>>;
	#incomingUnidirectionalQueue: IncomingStreamQueue<ReadableStream<Uint8Array>>;

	readonly #datagrams = makeDatagrams((data) => this.#sendDatagram(data));

	/** Unreliable datagrams sent and received as QMux DATAGRAM frames. */
	get datagrams(): WebTransportDatagramDuplexStream {
		return this.#datagrams;
	}

	// Flow control state
	#config: Required<Config>;
	#ourParams: TransportParams;
	#peerParams: TransportParams = { ...DEFAULT_TRANSPORT_PARAMS };
	#paramsReceived = false;
	// Settles once the peer's stream limits are known (or the session ends), so a
	// create that refuses to wait judges the peer's real limit, not the zero placeholder.
	#paramsSettled = Promise.withResolvers<void>();

	// Peer credit is unavailable until its transport parameters arrive.
	#connCredit = new Credit(0n);

	// Connection-level recv flow control
	#recvDataOffset = 0n;
	#recvDataMax = 0n;
	#recvDataConsumed = 0n;

	// Per-stream flow control
	#streamFlow = new Map<bigint, StreamFlowState>();
	#recvOpenBi = new RecvOpen();
	#recvOpenUni = new RecvOpen();

	// Stream counts start at zero until the peer grants credit.
	#bidiStreamCredit = new Credit(0n);
	#uniStreamCredit = new Credit(0n);

	// Stream count tracking via Credit (for receiving — our limits)
	#recvBiCredit: Credit;
	#recvUniCredit: Credit;

	// QMux01 idle-timeout tracking (engaged once we've received the peer's params).
	#lastRecvAt = now();
	#lastSendAt = now();
	// Deadline bookkeeping for #idleActivityAt: the newest receive observed, the
	// send last allowed to restart the deadline, and whether a send may restart it.
	#recvSeen = Instant.zero;
	#sendResetAt = Instant.zero;
	#sendCredit = true;
	#nextPingSeq = 0;
	// Highest sequence seen in a received QX_PING request (draft-02 requires them
	// to strictly increase). Undefined until the first request arrives.
	#lastPingRecv?: bigint;
	#idleTimer?: Interval;

	/** Open a QMux session as the **client**, dialing `url`.
	 *
	 * The polyfill constructs the underlying `WebSocket` itself. Pass the
	 * application-level ALPNs in `options.protocols` plus a `versions`
	 * map saying which QMux wire-format version each bare ALPN rides on. The
	 * wire form `{qmux-VV}.{alpn}` is built automatically; entries already in
	 * pair form (e.g. `"qmux-00.moq-transport-18"`) pass through unchanged.
	 *
	 * Once the handshake completes, the QMux wire-format version is derived
	 * from the negotiated `Sec-WebSocket-Protocol`. `.protocol` exposes the
	 * application protocol with the QMux prefix stripped.
	 *
	 * Pass an already-connected `WebSocket` (or `WebSocketStream`) instead of a
	 * URL to run the client role over a socket you opened yourself — the
	 * subprotocol was chosen when you created it, so `options.protocols` and
	 * `options.versions` are ignored. See [[Session.accept]] for the server role.
	 */
	constructor(source: string | URL | WebSocketLike | WebSocketStreamLike, options?: SessionOptions) {
		if (options?.requireUnreliable) {
			throw new Error("not allowed to use WebSocket; requireUnreliable is true");
		}
		// Pinning can't be honored over WebSocket, and connecting anyway would
		// silently drop the authentication the caller asked for.
		if (options?.serverCertificateHashes?.length) {
			throw new Error("not allowed to use WebSocket; serverCertificateHashes is not supported");
		}

		const dial = typeof source === "string" || source instanceof URL;
		// Resolve before any other setup so a bad protocols/versions pairing throws
		// from the constructor rather than failing the handshake later.
		const subprotocols = dial
			? resolveSubprotocols(options?.protocols ?? [], options?.versions ?? {}, options?.requireProtocol ?? false)
			: [];

		this.#config = resolveConfig(options?.config);
		this.#requireProtocol = dial && (options?.requireProtocol ?? false);
		this.#ourParams = configToTransportParams(this.#config);
		this.#sendBufferSize = options?.sendBufferSize ?? DEFAULT_SEND_BUFFER_SIZE;

		// Recv stream count limits are version-independent.
		this.#recvBiCredit = new Credit(this.#config.maxStreamsBidi);
		this.#recvUniCredit = new Credit(this.#config.maxStreamsUni);

		const ready = Promise.withResolvers<void>();
		this.ready = ready.promise;
		this.#readyResolve = ready.resolve;
		this.#readyReject = ready.reject;
		// Avoid an unhandled rejection if `ready` is rejected (early close) before
		// the caller attaches a handler; real awaiters still observe the rejection.
		this.ready.catch(() => {});

		const closed = Promise.withResolvers<WebTransportCloseInfo>();
		this.closed = closed.promise;
		this.#closedResolve = closed.resolve;
		this.#closedReject = closed.reject;
		// Same guard as `ready`: an abnormal close rejects `closed`, and a consumer
		// that only ever calls close() never attaches a handler. Real awaiters still
		// observe the rejection.
		this.closed.catch(() => {});

		this.#incomingBidirectionalQueue = new IncomingStreamQueue<WebTransportBidirectionalStream>();
		this.incomingBidirectionalStreams = this.#incomingBidirectionalQueue.readable;

		this.#incomingUnidirectionalQueue = new IncomingStreamQueue<ReadableStream<Uint8Array>>();
		this.incomingUnidirectionalStreams = this.#incomingUnidirectionalQueue.readable;

		this.#attach(
			dial
				? // Open the transport via `WebSocketStream` — native when present (real
					// backpressure), else the ponyfill over a plain `WebSocket`. One code
					// path for both, so the path exercised in tests is the one Chromium
					// runs natively.
					openWebSocketStream(toWebSocketUrl(source as string | URL), {
						protocols: subprotocols,
						highWaterMark: this.#sendBufferSize,
					})
				: toWebSocketStream(source as WebSocketLike | WebSocketStreamLike, this.#sendBufferSize),
		);
	}

	/** Open a QMux session as the **server**, over a socket you have already
	 *  accepted from an HTTP upgrade.
	 *
	 * The host performs the WebSocket handshake (`Deno.upgradeWebSocket`, `ws`,
	 * ...), so by the time you get here the subprotocol is already negotiated:
	 * the wire-format version and `.protocol` are read off the socket rather than
	 * advertised. Use [[selectSubprotocol]] to pick that value during the upgrade.
	 *
	 * The returned session is a `WebTransport`, identical to a client session
	 * apart from the stream-id role — so it accepts peer-initiated streams and
	 * opens server-initiated ones.
	 *
	 * @example
	 * ```ts
	 * const { socket, response } = Deno.upgradeWebSocket(req, { protocol });
	 * const session = Session.accept(socket, { config });
	 * await session.ready;
	 * ```
	 */
	static accept(socket: WebSocketLike | WebSocketStreamLike, options?: AcceptOptions): Session {
		const session = new Session(socket, options);
		// Applied after construction, which is safe because both transports settle
		// `opened` asynchronously: #startSession, every stream id, and every inbound
		// frame are interpreted strictly later than this.
		session.#isServer = true;
		session.#protocolOverride = options?.protocol;
		return session;
	}

	/** Drive the session off an opened transport, whichever way we got it. */
	#attach(wss: WebSocketStreamLike) {
		this.#wss = wss;

		wss.opened.then(
			(conn) => {
				// The session may have been closed before the socket finished opening.
				if (this.#closed) return;
				try {
					const protocol = this.#protocolOverride ?? conn.protocol;
					// Strict application negotiation also refuses a bare QMux version.
					if (this.#requireProtocol && parseProtocol(protocol, detectVersion(protocol)) === "") {
						this.#closeReason ??= new Error(
							`server negotiated no application protocol (${JSON.stringify(protocol)})`,
						);
						this.#abort(1002, "no application protocol");
						this.#transportClose();
						return;
					}
					this.#startSession(protocol, new WritableStreamSink(conn.writable));
					void this.#readLoop(conn.readable.getReader());
				} catch (error) {
					this.#closeReason ??= error instanceof Error ? error : new Error("Invalid QMux subprotocol");
					this.#abort(1002, "Invalid QMux subprotocol");
					this.#transportClose();
				}
			},
			(err: unknown) => {
				this.#closeReason ??= err instanceof Error ? err : new Error("WebSocketStream failed to open");
				this.#abort(1006, "WebSocketStream error");
			},
		);
		wss.closed.then(
			(info) => {
				// The socket went away without a CONNECTION_CLOSE. A peer close or a
				// local close() would already have transitioned us (and #close is
				// idempotent), so reaching here settled means the session dropped —
				// even if the WebSocket itself closed with a "clean" status code.
				this.#closeReason ??= new Error(`Connection closed: ${info.closeCode ?? 0} ${info.reason ?? ""}`);
				this.#abort(info.closeCode ?? 1006, info.reason ?? "");
			},
			(err: unknown) => {
				// Keep the socket failure itself — it becomes the SessionError's cause, and
				// it's the only description of *why* the transport died.
				this.#closeReason ??= err instanceof Error ? err : new Error("WebSocketStream closed");
				this.#abort(1006, "WebSocketStream error");
			},
		);
	}

	async #readLoop(reader: ReadableStreamDefaultReader<Uint8Array | ArrayBuffer | string>) {
		try {
			while (true) {
				const { value, done } = await reader.read();
				if (done) break;
				// QMux is binary-only; a text frame is a protocol error, not something
				// to silently drop (which would desync the session).
				if (typeof value === "string") {
					this.#sendClose("connection_close", 1003, "text frames are not valid for QMux");
					this.#abort(1003, "text frames are not valid for QMux");
					return;
				}
				// `openWebSocketStream` normalizes the native API's ArrayBuffer chunks,
				// but a transport handed straight to the constructor or [[accept]]
				// hasn't been through it — and Chromium's WebSocketStream yields
				// ArrayBuffer. Normalize here so every inbound path reaches the decoder
				// as a Uint8Array.
				this.#onData(value instanceof ArrayBuffer ? new Uint8Array(value) : value);
			}
		} catch (err) {
			this.#closeReason ??= err instanceof Error ? err : new Error("WebSocketStream read error");
			this.#abort(1006, "WebSocketStream read error");
		}
	}

	/** Derive the wire-format version, start the send scheduler, and (for QMux
	 *  drafts) exchange transport parameters. Shared by both transports. */
	#startSession(rawProtocol: string, sink: SendSink) {
		const version = detectVersion(rawProtocol);
		this.#version = version;
		this.#protocol = parseProtocol(rawProtocol, version);

		// Datagrams rely on the record layer for framing, so don't advertise the
		// parameter — or accept datagrams — on any non-record wire format. The recv
		// path gates on #ourParams.maxDatagramFrameSize. RESET_STREAM_AT is only
		// permitted on draft-02, so advertise `reset_stream_at` there and nowhere
		// else; the recv path gates on #ourParams.resetStreamAt.
		this.#ourParams = {
			...this.#ourParams,
			maxDatagramFrameSize: usesRecords(version) ? this.#ourParams.maxDatagramFrameSize : 0n,
			resetStreamAt: version === "qmux-02",
		};

		// QMux01 over WebSocket uses the WS message boundary as the implicit record
		// boundary; no extra size prefix is required.
		const params = Frame.encode({ type: "transport_parameters", params: this.#ourParams }, this.#version);
		this.#validateRecordSize(params);
		this.#scheduler = new SendScheduler(sink, {
			first: params,
			onActivity: () => {
				this.#lastSendAt = now();
			},
			// A failed write leaves the session unable to send anything, even if the
			// socket hasn't reported closing yet.
			onFail: (err) => {
				this.#closeReason ??= err;
				this.#abort(1006, "WebSocketStream write error");
				this.#transportClose();
			},
		});

		// Exchange transport parameters before granting send credit.

		this.#connCredit.close();
		this.#bidiStreamCredit.close();
		this.#uniStreamCredit.close();
		this.#connCredit = new Credit(0n);
		this.#bidiStreamCredit = new Credit(0n);
		this.#uniStreamCredit = new Credit(0n);
		this.#recvDataMax = this.#ourParams.initialMaxData;
		// Our own deadline applies until the peer's parameters arrive, so a peer
		// that never sends them can't hold the session open.
		this.#startIdleTimerIfEnabled();

		this.#readyResolve();
	}

	#onData(data: Uint8Array) {
		this.#lastRecvAt = now();
		try {
			if (usesRecords(this.#version)) {
				if (BigInt(data.byteLength) > this.#ourParams.maxRecordSize) {
					throw new Error(
						`record exceeds our max_record_size (${data.byteLength} > ${this.#ourParams.maxRecordSize})`,
					);
				}
				// Record-framed drafts: each WS message is a record with one or more frames
				const frames = Frame.decodeRecord(data);
				for (const frame of frames) {
					this.#recvFrame(frame);
				}
			} else {
				// draft-00 §5.2: `max_frame_size` bounds the whole frame, header
				// included, and may only be raised from its initial value — which we
				// never do, so the initial value is what a peer may send. Each message
				// is one frame here.
				if (this.#version === "qmux-00" && data.byteLength > Frame.MAX_FRAME_SIZE) {
					throw new Error(`frame exceeds max_frame_size (${data.byteLength} > ${Frame.MAX_FRAME_SIZE})`);
				}
				const frame = Frame.decode(data, this.#version);
				if (frame !== null) {
					this.#recvFrame(frame);
				}
			}
		} catch (error) {
			// A decode failure or a frame that violates a negotiated limit is fatal.
			console.error("Protocol violation:", error);
			this.#sendClose("connection_close", 1002, "Protocol violation");
			this.#abort(1002, "Protocol violation", error);
		}
	}

	#recvFrame(frame: Frame.Any) {
		// Draft-02: QX_TRANSPORT_PARAMETERS MUST be the first frame, and only the
		// first. A non-params frame before params, or a second params frame, is a
		// PROTOCOL_VIOLATION. (decodeRecord drops leading PADDING before we get here.)
		if (this.#version === "qmux-02") {
			const isParams = frame.type === "transport_parameters";
			if (isParams === this.#paramsReceived) {
				throw new Error("QX_TRANSPORT_PARAMETERS must be the first frame");
			}
		}

		if (frame.type === "stream") {
			this.#handleStreamFrame(frame);
		} else if (frame.type === "reset_stream") {
			this.#handleResetStream(frame);
		} else if (frame.type === "stop_sending") {
			this.#handleStopSending(frame);
		} else if (frame.type === "application_close" || frame.type === "connection_close") {
			this.#closeReason ??= new Error(`Connection closed: ${frame.code.value} ${frame.reason}`);
			if (frame.type === "application_close") {
				// APPLICATION_CLOSE (0x1d): the peer closed the session deliberately —
				// graceful, so `closed` fulfills with its code/reason.
				this.#close(Number(frame.code.value), frame.reason);
			} else {
				// CONNECTION_CLOSE (0x1c): the peer hit a protocol violation or
				// transport error, so `closed` rejects. A termination with *no* close
				// frame at all is a drop, which #abort also handles.
				this.#abort(Number(frame.code.value), frame.reason);
			}
			this.#transportClose();
		} else if (frame.type === "transport_parameters") {
			this.#handleTransportParameters(frame.params);
		} else if (frame.type === "max_data") {
			this.#connCredit.increaseMax(frame.max);
		} else if (frame.type === "max_stream_data") {
			const flow = this.#streamFlow.get(frame.id.value.value);
			if (flow) flow.sendCredit.increaseMax(frame.max);
		} else if (frame.type === "max_streams_bidi") {
			this.#bidiStreamCredit.increaseMax(frame.max);
		} else if (frame.type === "max_streams_uni") {
			this.#uniStreamCredit.increaseMax(frame.max);
		} else if (frame.type === "datagram") {
			// `max_datagram_frame_size` is negotiated: we advertised the largest
			// DATAGRAM frame we're willing to receive, so a peer that sends one we
			// never accepted — or one larger than we advertised — has violated the
			// contract. Per RFC 9221 that's a fatal PROTOCOL_VIOLATION, not a
			// droppable frame; throwing here closes the session via #onData (matching
			// the Rust implementation's DatagramsUnsupported / FrameTooLarge).
			//
			// The limit bounds the whole frame, whose size depends on the wire form:
			// the no-length `0x30` form is just a type byte plus payload, while `0x31`
			// adds a length varint. The decoder records which arrived so we can size
			// it exactly.
			if (this.#ourParams.maxDatagramFrameSize === 0n) {
				throw new Error("received a DATAGRAM but did not advertise datagram support");
			}
			const len = frame.data.byteLength;
			const header = frame.lengthPrefixed === false ? 1 : 1 + VarInt.from(len).size();
			const frameSize = BigInt(header + len);
			if (frameSize > this.#ourParams.maxDatagramFrameSize) {
				throw new Error("received a DATAGRAM larger than our advertised max_datagram_frame_size");
			}
			pushDatagram(this.#datagrams, frame.data);
		} else if (frame.type === "ping_request") {
			// Draft-02: request sequence numbers must strictly increase.
			if (this.#version === "qmux-02") {
				if (this.#lastPingRecv !== undefined && frame.sequence <= this.#lastPingRecv) {
					throw new Error("QX_PING request sequence must strictly increase");
				}
				this.#lastPingRecv = frame.sequence;
			}
			// Respond to ping requests
			this.#sendPriorityFrame({ type: "ping_response", sequence: frame.sequence });
		} else if (frame.type === "ping_response") {
			// Draft-02: a response must echo a sequence we actually sent (0..nextPingSeq).
			if (this.#version === "qmux-02" && frame.sequence >= BigInt(this.#nextPingSeq)) {
				throw new Error("QX_PING response echoed a sequence we never sent");
			}
		} else if (
			frame.type === "data_blocked" ||
			frame.type === "stream_data_blocked" ||
			frame.type === "streams_blocked_bidi" ||
			frame.type === "streams_blocked_uni"
		) {
			// Informational, no action needed
		}
	}

	#handleTransportParameters(params: TransportParams) {
		if (this.#paramsReceived) return;
		// Record-framed drafts: an advertised max_record_size MUST NOT go below the
		// default minimum. Omitted values decode to the default, so only an explicit
		// smaller value trips this (TRANSPORT_PARAMETER_ERROR).
		if (usesRecords(this.#version) && params.maxRecordSize < Frame.DEFAULT_MAX_RECORD_SIZE) {
			throw new Error("max_record_size below the default minimum");
		}
		this.#paramsReceived = true;
		this.#peerParams = params;
		this.#paramsSettled.resolve();

		this.#connCredit.increaseMax(params.initialMaxData);
		this.#bidiStreamCredit.increaseMax(params.initialMaxStreamsBidi);
		this.#uniStreamCredit.increaseMax(params.initialMaxStreamsUni);

		// Resolve the datagram send limit. Datagrams are a QMux01-only feature, so
		// they stay disabled on any other wire format. Otherwise whether we may
		// *send* depends solely on the peer's willingness to receive (RFC 9221):
		// 0 means unsupported.
		if (usesRecords(this.#version) && params.maxDatagramFrameSize > 0n) {
			// A datagram must fit in one record, so the frame is capped by the
			// smaller of the peer's datagram-frame limit and its record size.
			const cap =
				// biome-ignore lint/complexity/useMathMinMax: bigint operands; Math.min/max throw on bigint.
				params.maxRecordSize < params.maxDatagramFrameSize ? params.maxRecordSize : params.maxDatagramFrameSize;
			// We encode the length-prefixed form (0x31): one type byte plus a length
			// varint sized for `cap`. Subtracting it keeps the frame within the limit.
			const overhead = BigInt(1 + VarInt.from(cap).size());
			const payload = cap > overhead ? cap - overhead : 0n;
			setMaxDatagramSize(this.#datagrams, Number(payload));
		}

		// Credit streams created before params arrived. Only qmux-02 requires the
		// params first, so an earlier draft's peer may already have opened streams:
		// the peer's bidi_local limits its own bidi streams, bidi_remote ours.
		for (const [streamIdVal, flow] of this.#streamFlow) {
			const id = new Stream.Id(VarInt.from(streamIdVal));
			let sendLimit = params.initialMaxStreamDataUni;
			if (id.dir === Stream.Dir.Bi) {
				sendLimit =
					id.serverInitiated === this.#isServer
						? params.initialMaxStreamDataBidiRemote
						: params.initialMaxStreamDataBidiLocal;
			}
			flow.sendCredit.increaseMax(sendLimit);
		}

		// The peer's limit may be shorter than ours.
		this.#startIdleTimerIfEnabled();
	}

	/** Effective idle timeout in ms, or 0 if disabled.
	 *
	 * Per RFC 9000 §10.1, the effective value is `min(our, peer)` of the non-zero advertised values
	 * (or the single non-zero one). If both are zero, idle timeouts are disabled.
	 */
	#effectiveIdleTimeoutMs(): bigint {
		if (!usesRecords(this.#version)) return 0n;
		const a = this.#ourParams.maxIdleTimeout;
		const b = this.#peerParams.maxIdleTimeout;
		if (a === 0n && b === 0n) return 0n;
		if (a === 0n) return b;
		if (b === 0n) return a;
		// biome-ignore lint/complexity/useMathMinMax: bigint operands; Math.min/max throw on bigint.
		return a < b ? a : b;
	}

	#startIdleTimerIfEnabled() {
		this.#idleTimer?.close();
		this.#idleTimer = undefined;
		const timeoutMs = this.#effectiveIdleTimeoutMs();
		if (timeoutMs === 0n) return;
		// Poll at a fraction of the timeout — frequent enough to trigger pings on time
		// but not so frequent it burns CPU on otherwise-quiet sessions.
		const timeout = Nano.fromMilli(Milli(Number(timeoutMs)));
		const timer = interval(Nano.fromMilli(Milli(Math.max(50, Number(timeoutMs) / 6))));
		timer.setMissedTickBehavior(MissedTickBehavior.Delay);
		this.#idleTimer = timer;
		void this.#idleLoop(timer, timeout);
	}

	async #idleLoop(timer: Interval, timeout: Nano) {
		// The first tick is immediate; the first check belongs one period in.
		let first = true;
		for (;;) {
			try {
				await timer.tick();
			} catch {
				return; // #teardown closed the timer.
			}
			if (this.#closed) {
				timer.close();
				return;
			}
			if (!first) this.#idleTick(timeout);
			first = false;
		}
	}

	/**
	 * Newest activity that counts toward the idle deadline.
	 *
	 * A received frame always restarts the timer: it is the only direct proof the
	 * peer is still there. A send restarts it too, but at most once per receive
	 * (RFC 9000 §10.1). That proviso is what keeps the deadline reachable: our own
	 * keep-alive pings advance `#lastSendAt`, so counting every send would let them
	 * restart the very deadline they exist to test, and a peer that goes silent
	 * while its socket still accepts our writes would never be reclaimed.
	 *
	 * Crediting the first send after each receive is what still lets a mostly
	 * one-way sender stay open — its peer answers the keep-alive, and each answer
	 * re-arms the credit.
	 */
	#idleActivityAt(): Instant {
		if (this.#lastRecvAt > this.#recvSeen) {
			this.#recvSeen = this.#lastRecvAt;
			this.#sendCredit = true;
		}
		if (this.#sendCredit && this.#lastSendAt > this.#sendResetAt) {
			this.#sendResetAt = this.#lastSendAt;
			this.#sendCredit = false;
		}
		// biome-ignore lint/complexity/useMathMinMax: bigint operands; Math.min/max throw on bigint.
		return this.#recvSeen > this.#sendResetAt ? this.#recvSeen : this.#sendResetAt;
	}

	#idleTick(timeout: Nano) {
		const at = now();
		if (Instant.durationSince(at, this.#idleActivityAt()) > timeout) {
			// No frames have been sent or received within the negotiated limit. Abnormal:
			// without a rejection this is indistinguishable from a graceful close(), which
			// also settles with closeCode 0.
			this.#closeReason ??= new Error("idle timeout");
			this.#abort(0, "idle timeout");
			this.#transportClose();
			return;
		}
		// Keep-alive: nudge the peer when our outbound side has been silent for a third
		// of the timeout. Any frame counts as activity, so this only fires when truly idle.
		// Nothing to keep alive before the peer has spoken at all.
		if (this.#paramsReceived && Instant.durationSince(at, this.#lastSendAt) > timeout / 3) {
			const seq = this.#nextPingSeq;
			// Monotonic (never wraps within a session's life): the draft-02
			// strictly-increasing rule requires it, and it must not wrap earlier
			// than the Rust peer's u64 counter or a valid ping would be rejected.
			this.#nextPingSeq += 1;
			try {
				this.#sendPriorityFrame({ type: "ping_request", sequence: BigInt(seq) });
			} catch (e) {
				// Best effort — if the send fails, the close path will fire shortly.
				// Log a breadcrumb so a never-fire encoder bug doesn't vanish silently.
				console.warn("qmux: keep-alive ping failed", e);
			}
		}
	}

	async #claimSendCredit(streamId: bigint, desired: bigint, signal: AbortSignal): Promise<bigint> {
		const flow = this.#streamFlow.get(streamId);
		if (!flow) return desired;

		while (true) {
			// 1. Try stream credit
			const streamClaimed = flow.sendCredit.tryClaim(desired);
			if (streamClaimed === 0n) {
				if (this.#closed) throw this.#closed;
				// Wait for stream credit, then release and retry to coordinate with conn credit
				const claimed = await flow.sendCredit.claim(desired, signal);
				flow.sendCredit.release(claimed);
				continue;
			}

			// 2. Try connection credit
			const connClaimed = this.#connCredit.tryClaim(streamClaimed);
			if (connClaimed === 0n) {
				flow.sendCredit.release(streamClaimed);
				if (this.#closed) throw this.#closed;
				const claimed = await this.#connCredit.claim(1n, signal);
				this.#connCredit.release(claimed);
				continue;
			}

			// Return excess stream credit if connection had less
			if (connClaimed < streamClaimed) {
				flow.sendCredit.release(streamClaimed - connClaimed);
			}

			return connClaimed;
		}
	}

	/** Check that a STREAM frame continues its stream, then charge it to flow
	 *  control. Throws on a gap, overlap, or duplicate; returns false when the
	 *  frame exceeds a receive window. */
	#accountRecv(frame: Frame.Data): boolean {
		const flow = this.#streamFlow.get(frame.id.value.value);
		// The transport is ordered, so a frame must start exactly where the last one
		// ended (draft-02 §4.1): there is nothing to reassemble, and anything else
		// would deliver bytes out of order or twice. An absent offset comes from a
		// sender released before offsets were encoded, whose frames are contiguous.
		if (flow && frame.offset !== undefined && frame.offset !== flow.recvOffset) {
			throw new Error(`STREAM offset ${frame.offset} does not continue the stream at ${flow.recvOffset}`);
		}

		const bytesN = BigInt(frame.data.byteLength);
		if (bytesN === 0n) return true;

		// Connection-level check
		if (this.#recvDataOffset + bytesN > this.#recvDataMax) {
			return false;
		}
		this.#recvDataOffset += bytesN;

		// Stream-level check
		if (flow) {
			if (flow.recvOffset + bytesN > flow.recvMax) {
				return false;
			}
			flow.recvOffset += bytesN;
		}

		return true;
	}

	/** Connection-level credit. Accounted when bytes are delivered to the
	 *  application or deliberately discarded, never merely on receipt. This keeps
	 *  completed-but-unread streams inside the aggregate receive-memory window.
	 *  `recvDataConsumed` is cumulative. */
	#accountConnConsumed(bytes: number | bigint) {
		if (bytes === 0 || bytes === 0n) return;
		this.#recvDataConsumed += BigInt(bytes);
		this.#maybeSendMaxData();
	}

	/** Stream-level credit. Accounted on *delivery* to the application (driven by
	 *  RecvStream.onConsume), so MAX_STREAM_DATA tracks the read rate and the peer
	 *  can't buffer more than one window ahead of a slow reader. `recvConsumed`
	 *  is cumulative. */
	#accountStreamConsumed(streamId: bigint, bytes: number) {
		if (bytes === 0) return;
		const flow = this.#streamFlow.get(streamId);
		if (flow) {
			flow.recvConsumed += BigInt(bytes);
			this.#maybeSendMaxStreamData(streamId, flow);
		}
	}

	#maybeSendMaxData() {
		const newMax = replenishWindow(this.#recvDataConsumed, this.#recvDataMax, this.#ourParams.initialMaxData);
		if (newMax !== null) {
			this.#recvDataMax = newMax;
			this.#sendPriorityFrame({ type: "max_data", max: newMax });
		}
	}

	#maybeSendMaxStreamData(streamId: bigint, flow: StreamFlowState) {
		const id = new Stream.Id(VarInt.from(streamId));

		let initialWindow: bigint;
		if (id.dir === Stream.Dir.Bi) {
			// Check if we initiated this stream
			initialWindow =
				id.serverInitiated === this.#isServer
					? this.#ourParams.initialMaxStreamDataBidiLocal
					: this.#ourParams.initialMaxStreamDataBidiRemote;
		} else {
			initialWindow = this.#ourParams.initialMaxStreamDataUni;
		}

		// `recvConsumed` only grows on delivery to the application, so a stalled
		// reader stops replenishing and the peer's send credit drains to the window.
		const newMax = replenishWindow(flow.recvConsumed, flow.recvMax, initialWindow);
		if (newMax !== null) {
			flow.recvMax = newMax;
			this.#sendPriorityFrame({ type: "max_stream_data", id, max: newMax });
		}
	}

	/** Replenish stream count credit for a peer-initiated stream and send MAX_STREAMS if needed. */
	#replenishStreamCredit(dir: Stream.DirType) {
		const credit = dir === Stream.Dir.Bi ? this.#recvBiCredit : this.#recvUniCredit;
		const newMax = credit.consume(1n);
		if (newMax !== null) {
			if (dir === Stream.Dir.Bi) {
				this.#sendPriorityFrame({ type: "max_streams_bidi", max: newMax });
			} else {
				this.#sendPriorityFrame({ type: "max_streams_uni", max: newMax });
			}
		}
	}

	#recvOpen(dir: Stream.DirType): RecvOpen {
		return dir === Stream.Dir.Bi ? this.#recvOpenBi : this.#recvOpenUni;
	}

	/** Account the previously-unseen tail declared by RESET_STREAM.
	 *
	 * Drafts through -02 were emitted by implementations that used an incorrect
	 * zero final size, so tolerate a value below the bytes already received. The
	 * stricter FINAL_SIZE_ERROR check starts with draft-03; meanwhile the larger
	 * of the two values still prevents a reset from undoing flow-control usage. */
	#accountReset(frame: Frame.ResetStream): bigint | null {
		const streamId = frame.id.value.value;
		const flow = this.#streamFlow.get(streamId);
		const received = flow?.recvOffset ?? 0n;
		// TODO(qmux-03): Once draft-03 is implemented, reject finalSize < received
		// (and conflicting terminal final sizes) with FINAL_SIZE_ERROR instead of
		// applying this draft-00/-01/-02 compatibility fallback.
		// biome-ignore lint/complexity/useMathMinMax: bigint operands; Math.min/max throw on bigint.
		const finalSize = frame.finalSize > received ? frame.finalSize : received;
		const gap = finalSize - received;

		const recvMax =
			flow?.recvMax ??
			(frame.id.dir === Stream.Dir.Bi
				? this.#ourParams.initialMaxStreamDataBidiRemote
				: this.#ourParams.initialMaxStreamDataUni);
		if (finalSize > recvMax || this.#recvDataOffset + gap > this.#recvDataMax) {
			return null;
		}

		this.#recvDataOffset += gap;
		if (flow) flow.recvOffset = finalSize;
		return gap;
	}

	/** Delete stream flow state only when both send and recv sides are gone. */
	#maybeDeleteStreamFlow(streamId: bigint) {
		if (!this.#sendStreams.has(streamId) && !this.#recvStreams.has(streamId)) {
			const flow = this.#streamFlow.get(streamId);
			if (flow) {
				flow.sendCredit.close();
				this.#streamFlow.delete(streamId);
			}
		}
	}

	#handleStreamFrame(frame: Frame.Data) {
		// The read loop can still deliver frames after #close() has torn down the
		// incoming-stream controllers and #recvStreams; drop them, matching the
		// #sendDatagram/#sendPriorityFrame post-close guards.
		if (this.#closed) return;

		const streamId = frame.id.value.value;

		if (!frame.id.canRecv(this.#isServer)) {
			throw new Error("Invalid stream ID direction");
		}

		let recv = this.#recvStreams.get(streamId);
		if (
			frame.id.serverInitiated !== this.#isServer &&
			!recv &&
			this.#recvOpen(frame.id.dir).isClosed(frame.id.index)
		) {
			return;
		}

		if (!recv) {
			if (frame.id.serverInitiated === this.#isServer) {
				// One of ours whose receive half already closed: a late frame, dropped.
				// One we never opened is malformed (canRecv leaves only bidi here).
				if (frame.id.index >= this.#nextBiStreamId) {
					throw new Error("STREAM frame for a stream we never opened");
				}
				return;
			}
			if (!frame.id.canRecv(this.#isServer)) {
				throw new Error("received write-only stream");
			}

			// Validate stream count limits (QMux only)
			// Per QUIC RFC 9000 §4.6, the limit applies to the stream index.
			// A peer opening stream index N implicitly opens all streams 0..N.

			const credit = frame.id.dir === Stream.Dir.Bi ? this.#recvBiCredit : this.#recvUniCredit;
			if (!credit.receiveUpTo(frame.id.index + 1n)) {
				this.#sendClose("connection_close", 1002, "stream limit exceeded");
				this.#abort(1002, "stream limit exceeded");
				return;
			}
			this.#recvOpen(frame.id.dir).record(frame.id.index);

			// Initialize flow control state for new stream

			const recvMax =
				frame.id.dir === Stream.Dir.Bi
					? this.#ourParams.initialMaxStreamDataBidiRemote
					: this.#ourParams.initialMaxStreamDataUni;

			// For send side on bidi: peer's bidi_local is our send limit
			const sendMax = frame.id.dir === Stream.Dir.Bi ? this.#peerParams.initialMaxStreamDataBidiLocal : 0n;

			// Stream credit returns once the application has the stream and both halves have
			// ended, as in QUIC, so it bounds the streams a peer holds open.
			let holds = frame.id.dir === Stream.Dir.Bi ? 3 : 2;
			const release = () => {
				if (--holds === 0) this.#replenishStreamCredit(frame.id.dir);
			};

			this.#streamFlow.set(streamId, {
				sendCredit: new Credit(sendMax),
				sendOffset: 0n,
				recvMax,
				recvOffset: 0n,
				recvConsumed: 0n,
				onSendEnd: frame.id.dir === Stream.Dir.Bi ? release : undefined,
			});

			// Validate recv flow control before accepting
			if (!this.#accountRecv(frame)) {
				this.#sendClose("connection_close", 1002, "flow control error");
				this.#abort(1002, "flow control error");
				return;
			}

			const recvStream = new RecvStream(
				(bytes) => {
					this.#accountStreamConsumed(streamId, bytes);
					this.#accountConnConsumed(bytes);
				},
				(discarded, reason) => {
					this.#accountConnConsumed(discarded);
					this.#sendPriorityFrame({
						type: "stop_sending",
						id: frame.id,
						code: VarInt.from(resetCode(reason)),
					});

					this.#recvStreams.delete(streamId);
					this.#maybeDeleteStreamFlow(streamId);
				},
				release,
			);
			this.#recvStreams.set(streamId, recvStream);
			recv = recvStream;
			const reader = recvStream.readable;

			const onDrop = (err: Error) => {
				this.#accountConnConsumed(recvStream.error(err));
			};

			if (frame.id.dir === Stream.Dir.Bi) {
				// Incoming bidirectional stream
				const writer = this.#sendWritable(frame.id, DEFAULT_SEND_ORDER);

				if (!this.#incomingBidirectionalQueue.push({ readable: reader, writable: writer }, release, onDrop)) {
					// Keep the refused stream registered until FIN/RESET so in-flight frames
					// cannot repeatedly reopen it. RecvStream rejects subsequent pushes,
					// letting the receive path account them as deliberately discarded.
					onDrop(new Error("incoming stream acceptor cancelled"));
				}
			} else {
				if (!this.#incomingUnidirectionalQueue.push(reader, release, onDrop)) {
					onDrop(new Error("incoming stream acceptor cancelled"));
				}
			}
		} else {
			// Existing stream — validate recv flow control
			if (!this.#accountRecv(frame)) {
				this.#sendClose("connection_close", 1002, "flow control error");
				this.#abort(1002, "flow control error");
				return;
			}
		}

		if (frame.data.byteLength > 0) {
			// A refused/reset/cancelled stream discards new in-flight data. Discarded
			// bytes free memory immediately and therefore count as consumed; buffered
			// bytes advance both flow-control windows only when RecvStream delivers them.
			if (!recv.push(frame.data)) this.#accountConnConsumed(frame.data.byteLength);
		}

		if (frame.fin) {
			recv.finish();
			this.#recvStreams.delete(streamId);
			this.#maybeDeleteStreamFlow(streamId);
		}
	}

	#handleResetStream(frame: Frame.ResetStream) {
		// A RESET_STREAM_AT frame (reliableSize present, draft-02) is only legal if
		// we advertised the `reset_stream_at` transport parameter. Receiving it
		// otherwise — or on an earlier draft, which never advertises it — is a
		// PROTOCOL_VIOLATION. Plain RESET_STREAM is always allowed.
		if (frame.reliableSize !== undefined && !this.#ourParams.resetStreamAt) {
			throw new Error("RESET_STREAM_AT received without advertising reset_stream_at");
		}
		if (!frame.id.canRecv(this.#isServer)) {
			throw new Error("Invalid stream ID direction");
		}

		const streamId = frame.id.value.value;
		const recv = this.#recvStreams.get(streamId);
		const peerInitiated = frame.id.serverInitiated !== this.#isServer;

		if (!recv) {
			// A terminal peer-initiated stream stays terminal. In particular, a
			// duplicate RESET must not consume its final size twice.
			if (peerInitiated && this.#recvOpen(frame.id.dir).isClosed(frame.id.index)) {
				return;
			}
			// A locally-created receive half that is no longer live is already closed,
			// unless we never opened it at all.
			if (!peerInitiated) {
				const opened = frame.id.dir === Stream.Dir.Bi ? this.#nextBiStreamId : this.#nextUniStreamId;
				if (frame.id.index >= opened) throw new Error("RESET_STREAM for a stream we never opened");
				return;
			}

			const credit = frame.id.dir === Stream.Dir.Bi ? this.#recvBiCredit : this.#recvUniCredit;
			if (!credit.receiveUpTo(frame.id.index + 1n)) {
				this.#sendClose("connection_close", 1002, "stream limit exceeded");
				this.#abort(1002, "stream limit exceeded");
				return;
			}
		}

		// RESET_STREAM_AT (draft-02 §9.2): the transport is ordered, so every byte
		// the peer sent arrived before this frame. A reliable prefix longer than
		// that promises bytes that will never come.
		if (
			frame.reliableSize !== undefined &&
			frame.reliableSize > (this.#streamFlow.get(streamId)?.recvOffset ?? 0n)
		) {
			throw new Error("RESET_STREAM_AT reliable_size exceeds the bytes received");
		}

		const resetGap = this.#accountReset(frame);
		if (resetGap === null) {
			this.#sendClose("connection_close", 1002, "flow control error");
			this.#abort(1002, "flow control error");
			return;
		}

		if (!recv) {
			// The final-size gap consumes connection flow control, but it can never
			// occupy receive memory, so it is immediately eligible to replenish the
			// connection window.
			this.#accountConnConsumed(resetGap);
			this.#recvOpen(frame.id.dir).record(frame.id.index);
			this.#replenishStreamCredit(frame.id.dir);
			return;
		}

		// Only bytes past the reliable prefix are discarded; the reader still gets
		// the prefix before the reset surfaces.
		const discarded = recv.reset(new StreamError(streamCode(frame.code.value), "RESET_STREAM"), frame.reliableSize);
		this.#accountConnConsumed(resetGap + BigInt(discarded));
		this.#recvStreams.delete(streamId);
		this.#maybeDeleteStreamFlow(streamId);
	}

	#handleStopSending(frame: Frame.StopSending) {
		const streamId = frame.id.value.value;
		const stream = this.#sendStreams.get(streamId);
		if (!stream) return;

		const stopped = new StreamError(streamCode(frame.code.value), "STOP_SENDING");
		stream.error(stopped);
		this.#scheduler?.dropStream(streamId, stopped);
		// A write parked on stream credit isn't interrupted by error(); fail it here,
		// even though a live receive half keeps the flow state around.
		this.#streamFlow.get(streamId)?.sendCredit.close(stopped);

		this.#sendPriorityFrame({
			type: "reset_stream",
			id: frame.id,
			code: frame.code,
			finalSize: this.#sendFinalSize(streamId),
		});

		this.#endSend(streamId);
	}

	/** End a stream's send half once its FIN or RESET_STREAM is sent. */
	#endSend(streamId: bigint) {
		if (!this.#sendStreams.delete(streamId)) return;
		this.#streamFlow.get(streamId)?.onSendEnd?.();
		this.#maybeDeleteStreamFlow(streamId);
	}

	#sendFinalSize(streamId: bigint): bigint {
		return this.#streamFlow.get(streamId)?.sendOffset ?? 0n;
	}

	/** The largest STREAM frame the peer accepts, in bytes.
	 *
	 * Record-framed drafts negotiate it: a frame rides in one record, so the
	 * peer's `max_record_size` is the limit (the draft-01 default until its
	 * TRANSPORT_PARAMETERS arrive, so we never send something it would reject).
	 * Draft-00 has no record layer,
	 * so draft-00's whole-frame `max_frame_size` stands in. */
	#sendFrameBudget(): bigint {
		if (!usesRecords(this.#version)) return BigInt(Frame.MAX_FRAME_SIZE);
		return this.#paramsReceived ? this.#peerParams.maxRecordSize : Frame.DEFAULT_MAX_RECORD_SIZE;
	}

	/** The largest payload we may send in one STREAM frame at `offset`.
	 *
	 * This respects the peer's frame budget and the send-only compatibility
	 * ceiling for released receivers. A `bigint`: the peer's `max_record_size` is
	 * a varint, so it can exceed what `number` holds exactly. Callers clamp it to
	 * the bytes they actually have before converting. */
	#maxStreamPayload(id: Stream.Id, offset: bigint): bigint {
		const peerMax = Frame.maxStreamPayload(this.#version, this.#sendFrameBudget(), id, offset);
		const compatibilityMax = BigInt(Frame.MAX_FRAME_PAYLOAD);
		// biome-ignore lint/complexity/useMathMinMax: bigint operands; Math.min/max throw on bigint.
		const max = peerMax < compatibilityMax ? peerMax : compatibilityMax;
		// Unreachable with a conforming peer: max_record_size is validated against
		// the draft-01 minimum on arrival. Guard anyway — a zero budget would make
		// the chunking loops below spin forever instead of failing.
		if (max === 0n) throw new Error("peer frame limit leaves no room for stream data");
		return max;
	}

	/** Validate an encoded record against the peer's max_record_size (QMux01+). */
	#validateRecordSize(bytes: Uint8Array) {
		if (usesRecords(this.#version)) {
			// Before the peer's TRANSPORT_PARAMETERS arrive, use the draft-01 default
			// (16382) so we don't accidentally send something the peer will reject.
			const limit = this.#paramsReceived ? this.#peerParams.maxRecordSize : Frame.DEFAULT_MAX_RECORD_SIZE;
			if (BigInt(bytes.byteLength) > limit) {
				throw new Error(`record exceeds peer max_record_size (${bytes.byteLength} > ${limit})`);
			}
		}
	}

	/** Encode and enqueue a stream-data/fin frame, resolving once it hits the wire.
	 *  Refuses once `signal` aborts: its listener drops only what is already queued. */
	async #enqueueStreamFrame(streamId: bigint, frame: Frame.Data, signal?: AbortSignal) {
		signal?.throwIfAborted();
		const scheduler = this.#scheduler;
		if (!scheduler) throw this.#closed ?? new Error("session not open");
		const bytes = Frame.encode(frame, this.#version);
		this.#validateRecordSize(bytes);
		await scheduler.enqueueStream(streamId, bytes);
	}

	async #sendStreamDataWithFlowControl(id: Stream.Id, streamId: bigint, data: Uint8Array, signal: AbortSignal) {
		const flow = this.#streamFlow.get(streamId);
		if (!flow) throw new Error(`missing flow state for stream ${streamId}`);

		for (let offset = 0; offset < data.byteLength; ) {
			const remaining = BigInt(data.byteLength - offset);
			// Size from the peer's frame budget and this frame's actual header, then
			// apply the compatibility ceiling for released receivers.
			const payloadMax = this.#maxStreamPayload(id, flow.sendOffset);
			// biome-ignore lint/complexity/useMathMinMax: bigint operands; Math.min/max throw on bigint.
			const chunkMax = Number(payloadMax < remaining ? payloadMax : remaining);

			// Claim flow control credit (stream + connection)
			const allowed = await this.#claimSendCredit(streamId, BigInt(chunkMax), signal);
			const sendable = Number(allowed);

			const chunk = data.subarray(offset, offset + sendable);
			const sendOffset = flow.sendOffset;

			try {
				await this.#enqueueStreamFrame(
					streamId,
					{ type: "stream", id, offset: sendOffset, data: chunk, fin: false },
					signal,
				);
			} catch (e) {
				// Return claimed credits on send failure
				if (sendable > 0) {
					const flow = this.#streamFlow.get(streamId);
					if (flow) flow.sendCredit.release(BigInt(sendable));
					this.#connCredit.release(BigInt(sendable));
				}
				throw e;
			}

			flow.sendOffset += BigInt(sendable);
			offset += sendable;
		}
	}

	async #sendStreamData(id: Stream.Id, data: Uint8Array, signal: AbortSignal) {
		const streamId = id.value.value;

		await this.#sendStreamDataWithFlowControl(id, streamId, data, signal);
	}

	/** Send the FIN. Routed through the stream's own queue so it stays ordered
	 *  after that stream's data (not via the control lane, which would jump ahead). */
	async #sendStreamFin(id: Stream.Id) {
		const streamId = id.value.value;
		const offset = this.#streamFlow.get(streamId)?.sendOffset;
		if (offset === undefined) {
			throw new Error(`missing flow state for stream ${streamId}`);
		}
		await this.#enqueueStreamFrame(streamId, { type: "stream", id, offset, data: new Uint8Array(), fin: true });
	}

	/** Enqueue a DATAGRAM frame on the scheduler's bounded, lossy datagram lane —
	 *  dropped under transport backpressure or once closed, rather than piling up
	 *  on the (lossless, unbounded) control lane. Size/support checks happen in
	 *  {@link Datagrams} before we get here. */
	#sendDatagram(data: Uint8Array) {
		if (this.#closed) return;
		const bytes = Frame.encode({ type: "datagram", data }, this.#version);
		this.#validateRecordSize(bytes);
		this.#scheduler?.enqueueDatagram(bytes);
	}

	#sendPriorityFrame(frame: Frame.Any) {
		// Once closed, the scheduler rejects new control frames; a late reset/stop
		// from a stream teardown callback must be a no-op, not a throw. (The close
		// frame goes through #sendClose instead.)
		if (this.#closed) return;
		const bytes = Frame.encode(frame, this.#version);
		this.#validateRecordSize(bytes);
		if (frame.type === "ping_request" || frame.type === "ping_response") {
			// Only the newest ping matters: answering the latest request proves
			// liveness for all before it, and a newer keep-alive makes an unsent one
			// moot. Coalescing keeps a peer that pings without reading from growing
			// the control backlog.
			this.#scheduler?.replaceControl(frame.type, bytes);
		} else {
			this.#scheduler?.enqueueControl(bytes);
		}
	}

	/** Take one of the peer's stream slots for a new outgoing stream.
	 *
	 *  With `waitUntilAvailable: false` an exhausted limit rejects at once, the way
	 *  Chrome's WebTransport does, instead of queueing behind the peer's next
	 *  MAX_STREAMS. It still waits for the peer's TRANSPORT_PARAMETERS, the
	 *  counterpart of the QUIC handshake that tells Chrome the limit up front. */
	async #claimStream(dir: Stream.DirType, options: SendStreamOptions | undefined, refused: string) {
		await this.ready;
		if (this.#closed) throw this.#closed;

		if (options?.waitUntilAvailable === false) {
			await this.#paramsSettled.promise;
			if (this.#closed) throw this.#closed;
			const credit = dir === Stream.Dir.Bi ? this.#bidiStreamCredit : this.#uniStreamCredit;
			if (credit.tryClaim(1n) === 0n) throw new DOMException(refused, "NetworkError");
			return;
		}

		await (dir === Stream.Dir.Bi ? this.#bidiStreamCredit : this.#uniStreamCredit).claim(1n);
	}

	/** Register a stream's initial send priority and expose a mutable `sendOrder`
	 *  accessor on its writable (matching the W3C `WebTransportSendStream` API).
	 *  Updating it re-prioritizes the stream's queued data immediately. */
	#attachSendOrder(writable: WritableStream<Uint8Array>, streamId: bigint, initial: number) {
		this.#scheduler?.setSendOrder(streamId, initial);
		let order = initial;
		Object.defineProperty(writable, "sendOrder", {
			configurable: true,
			enumerable: true,
			get: () => order,
			set: (value: number) => {
				order = value;
				this.#scheduler?.setSendOrder(streamId, value);
			},
		});
	}

	/** Open a bidirectional stream, waiting for the peer to grant stream credit
	 *  unless `waitUntilAvailable` is false. */
	async createBidirectionalStream(options?: SendStreamOptions): Promise<WebTransportBidirectionalStream> {
		await this.#claimStream(Stream.Dir.Bi, options, "Failed to create bidirectional stream.");
		// close() may have torn down the session while the claim resumed.
		if (this.#closed) throw this.#closed;

		const sendOrder = options?.sendOrder ?? DEFAULT_SEND_ORDER;

		const streamId = Stream.Id.create(this.#nextBiStreamId++, Stream.Dir.Bi, this.#isServer);
		const streamIdVal = streamId.value.value;

		// Initialize flow control for this stream

		this.#streamFlow.set(streamIdVal, {
			sendCredit: new Credit(this.#peerParams.initialMaxStreamDataBidiRemote),
			sendOffset: 0n,
			recvMax: this.#ourParams.initialMaxStreamDataBidiLocal,
			recvOffset: 0n,
			recvConsumed: 0n,
		});

		const writer = this.#sendWritable(streamId, sendOrder);

		const recvStream = new RecvStream(
			(bytes) => {
				this.#accountStreamConsumed(streamIdVal, bytes);
				this.#accountConnConsumed(bytes);
			},
			(discarded, reason) => {
				this.#accountConnConsumed(discarded);
				this.#sendPriorityFrame({
					type: "stop_sending",
					id: streamId,
					code: VarInt.from(resetCode(reason)),
				});

				this.#recvStreams.delete(streamIdVal);
				this.#maybeDeleteStreamFlow(streamIdVal);
			},
		);
		this.#recvStreams.set(streamIdVal, recvStream);

		return { readable: recvStream.readable, writable: writer };
	}

	/** Open a send-only stream, waiting for the peer to grant stream credit
	 *  unless `waitUntilAvailable` is false. */
	async createUnidirectionalStream(options?: SendStreamOptions): Promise<WritableStream<Uint8Array>> {
		await this.#claimStream(Stream.Dir.Uni, options, "Failed to create send stream.");
		// close() may have torn down the session while the claim resumed.
		if (this.#closed) throw this.#closed;

		const sendOrder = options?.sendOrder ?? DEFAULT_SEND_ORDER;

		const streamId = Stream.Id.create(this.#nextUniStreamId++, Stream.Dir.Uni, this.#isServer);
		const streamIdVal = streamId.value.value;

		// Initialize flow control for this stream

		this.#streamFlow.set(streamIdVal, {
			sendCredit: new Credit(this.#peerParams.initialMaxStreamDataUni),
			sendOffset: 0n,
			recvMax: 0n,
			recvOffset: 0n,
			recvConsumed: 0n,
		});

		return this.#sendWritable(streamId, sendOrder);
	}

	/** The writable for a stream's send half. */
	#sendWritable(id: Stream.Id, sendOrder: number): WritableStream<Uint8Array> {
		const streamId = id.value.value;
		let signal!: AbortSignal;
		const reset = (e: unknown) => {
			this.#scheduler?.dropStream(streamId, e instanceof Error ? e : new Error("stream aborted"));
			this.#sendPriorityFrame({
				type: "reset_stream",
				id,
				code: VarInt.from(resetCode(e)),
				finalSize: this.#sendFinalSize(streamId),
			});

			this.#endSend(streamId);
		};
		const writable = new WritableStream<Uint8Array>({
			start: (controller) => {
				signal = controller.signal;
				this.#sendStreams.set(streamId, controller);
				// An abort fires the signal at once but runs `abort` below only after the
				// in-flight write settles, so the signal has to end that write: here if
				// it is queued behind other frames, in #claimSendCredit if it is parked
				// on the peer's credit.
				controller.signal.addEventListener(
					"abort",
					() => {
						const reason = controller.signal.reason;
						this.#scheduler?.dropStream(
							streamId,
							reason instanceof Error ? reason : new Error("stream aborted"),
						);
					},
					{ once: true },
				);
			},
			write: async (chunk, controller) => {
				await this.#sendStreamData(id, chunk, controller.signal);
			},
			abort: reset,
			close: async () => {
				try {
					await this.#sendStreamFin(id);
				} catch (e) {
					// An abort during close drops the queued FIN, and WritableStream never
					// runs `abort` once an in-flight close fails, so reset here instead.
					if (!signal.aborted) throw e;
					reset(signal.reason);
					return;
				}

				this.#scheduler?.forget(streamId);
				this.#endSend(streamId);
			},
		});
		this.#attachSendOrder(writable, streamId, sendOrder);
		return writable;
	}

	/** Shared teardown for both terminal transitions: tears down streams,
	 *  credits, and the scheduler. The caller has already marked the session
	 *  closed and settled `ready`/`closed` — the only thing that differs between
	 *  a clean shutdown and a dropped session is which way `closed` settled, and
	 *  {@link #close} vs {@link #abort} own that choice. */
	#teardown() {
		this.#idleTimer?.close();
		this.#idleTimer = undefined;

		// Fail active streams so consumers unblock
		this.#incomingBidirectionalQueue.close(this.#closeReason);
		this.#incomingUnidirectionalQueue.close(this.#closeReason);
		// Pass the *reason*, not `#closed` (always an Error): a graceful
		// app-initiated close leaves `#closeReason` unset, so the datagram
		// readable closes cleanly instead of erroring, matching the incoming
		// stream controllers below.
		closeDatagrams(this.#datagrams, this.#closeReason);
		for (const c of this.#sendStreams.values()) {
			try {
				c.error(this.#closed);
			} catch {}
		}
		const closeErr = this.#closed ?? this.#closeReason ?? new Error("Connection closed");
		for (const recv of this.#recvStreams.values()) {
			try {
				recv.error(closeErr);
			} catch {}
		}
		this.#sendStreams.clear();
		this.#recvStreams.clear();

		// Close per-stream credits before clearing the map. Pass the rich close
		// reason so writes parked in Credit.claim() reject with the same
		// "Connection closed: <code> <reason>" error as every other teardown path.
		for (const flow of this.#streamFlow.values()) {
			flow.sendCredit.close(closeErr);
		}
		this.#streamFlow.clear();

		this.#paramsSettled.resolve();

		// Close global credits so blocked claim() calls reject.
		this.#connCredit.close(closeErr);
		this.#bidiStreamCredit.close(closeErr);
		this.#uniStreamCredit.close(closeErr);
		this.#recvBiCredit.close(closeErr);
		this.#recvUniCredit.close(closeErr);

		// Drop whatever is still queued so the close frame goes out next.
		const flushed = this.#scheduler?.close(closeErr, this.#closeFrame) ?? Promise.resolve();
		if (this.#closeFrame) {
			// Close the socket once it has the close frame. A socket the peer stopped
			// draining can't hold it open: past CLOSE_TIMEOUT it closes without it.
			const close = () => this.#transportClose();
			void timeout(CLOSE_TIMEOUT, flushed).then(close, close);
		}
	}

	/** Graceful terminal transition: `closed` **fulfills** with the close code
	 *  and reason. Per the WebTransport contract this is the only clean outcome —
	 *  reached by a local {@link close} (which first puts an APPLICATION_CLOSE on
	 *  the wire) or by receiving a peer's APPLICATION_CLOSE (0x1d). A peer's
	 *  CONNECTION_CLOSE (0x1c) routes to {@link #abort} instead. Idempotent. */
	#close(code: number, reason: string) {
		if (this.#closed) return;
		this.#closed = this.#closeReason ?? new Error(`Connection closed: ${code} ${reason}`);

		// Rejecting `ready` is a no-op once it has resolved (i.e. after a
		// successful open).
		this.#readyReject(this.#closed);
		this.#closedResolve({ closeCode: code, reason });

		this.#teardown();
	}

	/** Abnormal terminal transition: `closed` **rejects** with a
	 *  `WebTransportError`. Reached on everything that is not a clean shutdown —
	 *  socket failure, read error, idle timeout, a protocol violation we detected
	 *  locally, or a peer's CONNECTION_CLOSE (0x1c). Close codes cannot carry this
	 *  distinction, since they are
	 *  application-defined (an app closing with 1006 must not look like a dropped
	 *  socket). `cause`, when given, is preserved as the close reason so the
	 *  original failure survives on `closed`. Idempotent. */
	#abort(code: number, reason: string, cause?: unknown) {
		if (this.#closed) return;
		if (cause !== undefined) {
			this.#closeReason ??= cause instanceof Error ? cause : new Error(String(cause));
		}
		this.#closed = this.#closeReason ?? new Error(`Connection closed: ${code} ${reason}`);

		this.#readyReject(this.#closed);
		this.#closedReject(new SessionError(this.#closed.message, { cause: this.#closed }));

		this.#teardown();
	}

	/** Tear down the underlying transport. The meaningful close code/reason
	 *  already travels in the CONNECTION_CLOSE frame, so the WebSocket-level
	 *  close is bare (avoids the WebSocket close-code validity constraints). */
	#transportClose() {
		try {
			this.#wss?.close();
		} catch {}
	}

	/** Tell the peer why the session ends, then close the socket; call just before
	 *  the terminal transition, whose teardown sends it. APPLICATION_CLOSE (0x1d)
	 *  is a graceful close the peer surfaces by *fulfilling* its `closed`, pairing
	 *  with {@link #close}. CONNECTION_CLOSE (0x1c) is a protocol violation or
	 *  transport error we detected, which the peer surfaces by *rejecting* it,
	 *  pairing with {@link #abort}. */
	#sendClose(type: "application_close" | "connection_close", code: number, reason: string) {
		if (this.#closed) return;
		const bytes = Frame.encode({ type, code: VarInt.from(code), reason }, this.#version);
		this.#validateRecordSize(bytes);
		this.#closeFrame = bytes;
	}

	/** Close the session with an application code and reason; a no-op once closed. */
	close(info?: { closeCode?: number; reason?: string }) {
		if (this.#closed) return;
		const code = info?.closeCode ?? 0;
		const reason = info?.reason ?? "";
		this.#sendClose("application_close", code, reason);
		this.#close(code, reason);
	}

	/** Resize the send-buffer high-water mark (bytes) used for write
	 *  backpressure. A QMux extension beyond the standard `WebTransport` API:
	 *  set this to roughly the bandwidth-delay product (RTT × estimated
	 *  throughput) to keep the pipe full while leaving as much queued data as
	 *  possible reprioritizable by the send scheduler.
	 *
	 *  No-op when a native `WebSocketStream` is in use (it sizes its own send
	 *  buffer); effective with the `WebSocketStream` ponyfill fallback. */
	setSendBufferSize(bytes: number) {
		this.#sendBufferSize = Math.max(1, Math.floor(bytes));
		this.#wss?.setHighWaterMark?.(this.#sendBufferSize);
	}

	/** Always `"default"`: the underlying TCP connection does its own congestion control. */
	get congestionControl(): WebTransportCongestionControl {
		return "default";
	}

	/** Always `"reliable-only"`: datagrams ride the WebSocket, so nothing is unreliable. */
	get reliability() {
		return "reliable-only" as const;
	}

	/** Reports no counters: the WebSocket exposes none. `datagrams` is required by the W3C dictionary. */
	async getStats() {
		return { datagrams: {} };
	}
}
