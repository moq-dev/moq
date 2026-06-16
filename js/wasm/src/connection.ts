// The `Connection` namespace, mirroring `@moq/net`'s `Connection.connect` /
// `Established` / `Reload`. The wire lives in wasm (`Session` + its
// `OriginConsumer`); this establishes it and presents the `@moq/net` shape.
//
// `Reload` is backend-agnostic reconnection + announce-aggregation logic, ported
// from `@moq/net` with the `connect` import pointed at the wasm one.

import { Path, type Time } from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";
import * as Wasm from "#bindgen";
import type { Announced, Broadcast } from "./index.ts";
import { init, Session } from "./index.ts";

/** A connected MoQ session. Structurally matches `@moq/net`'s `Connection.Established`. */
export interface Established {
	readonly url: URL;
	readonly version: string;

	// Telemetry, surfaced for `@moq/net` parity. Not yet bridged from wasm (undefined).
	readonly sendBandwidth?: Signal<number | undefined>;
	readonly recvBandwidth?: Signal<number | undefined>;
	readonly rtt?: Signal<Time.Milli | undefined>;

	consume(broadcast: string): Broadcast;
	publish(path: string, broadcast: Broadcast): void;
	announced(prefix?: Path.Valid): Announced;
	close(): void;
	closed: Promise<void>;
}

// WebTransport-only for now: the wasm transport has no WebSocket fallback, so
// these options are accepted for `@moq/net` parity but ignored.
export interface WebSocketOptions {
	enabled?: boolean;
	url?: URL;
	delay?: DOMHighResTimeStamp;
}

export interface ConnectProps {
	webtransport?: WebTransportOptions;
	websocket?: WebSocketOptions;
}

/**
 * Establish a connection to a MoQ relay over WebTransport.
 *
 * Over `http://` (local dev) this fetches the relay's self-signed certificate
 * fingerprint from `/certificate.sha256`, pins it, and upgrades the scheme to
 * `https://` (WebTransport requires https). Mirrors `@moq/net`. Pass explicit
 * `webtransport.serverCertificateHashes` for serverless dev; otherwise the
 * system roots are used.
 */
export async function connect(url: URL, props?: ConnectProps): Promise<Established> {
	await init();

	const hashes: Uint8Array[] = (props?.webtransport?.serverCertificateHashes ?? []).map(hashToBytes);

	let target = url;
	if (url.protocol === "http:") {
		const fingerprintUrl = new URL(url);
		fingerprintUrl.pathname = "/certificate.sha256";
		fingerprintUrl.search = "";
		console.warn(fingerprintUrl.toString(), "performing an insecure fingerprint fetch; use https:// in production");

		const res = await fetch(fingerprintUrl);
		hashes.push(hexToBytes((await res.text()).trim()));

		target = new URL(url);
		target.protocol = "https:";
	}

	const inner = hashes.length
		? await Wasm.Session.connectWithHashes(target.toString(), hashes)
		: await Wasm.Session.connect(target.toString());

	// Keep the original URL (e.g. http://localhost) for display, like @moq/net.
	return new Session(url, inner);
}

function hashToBytes(hash: WebTransportHash): Uint8Array {
	const value = hash.value;
	if (value instanceof ArrayBuffer) return new Uint8Array(value);
	// ArrayBufferView (e.g. Uint8Array): copy its viewed bytes.
	return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
}

function hexToBytes(hex: string): Uint8Array {
	const bytes = new Uint8Array(hex.length >> 1);
	for (let i = 0; i < bytes.length; i++) {
		bytes[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
	}
	return bytes;
}

export type ReloadDelay = {
	initial: DOMHighResTimeStamp;
	multiplier: number;
	max: DOMHighResTimeStamp;
	timeout?: DOMHighResTimeStamp;
};

export type ReloadProps = ConnectProps & {
	enabled?: boolean | Signal<boolean>;
	url?: URL | Signal<URL | undefined>;
	delay?: ReloadDelay;
};

export type ReloadStatus = "connecting" | "connected" | "disconnected";

/** Reconnects on drop and aggregates announce events into a reactive set. */
export class Reload {
	url: Signal<URL | undefined>;
	enabled: Signal<boolean>;

	status = new Signal<ReloadStatus>("disconnected");
	established = new Signal<Established | undefined>(undefined);

	#announced = new Signal<Set<Path.Valid>>(new Set());
	readonly announced: Getter<Set<Path.Valid>> = this.#announced;

	webtransport?: WebTransportOptions;
	websocket: WebSocketOptions | undefined;

	delay: ReloadDelay;

	signals = new Effect();

	closed: Promise<void>;
	#closedResolve!: () => void;
	#closedReject!: (err: Error) => void;

	#delay: DOMHighResTimeStamp;
	#retryStart: DOMHighResTimeStamp | undefined;
	#tick = new Signal(0);

	constructor(props?: ReloadProps) {
		this.url = Signal.from(props?.url);
		this.enabled = Signal.from(props?.enabled ?? false);
		this.delay = props?.delay ?? { initial: 1000, multiplier: 2, max: 30000 };
		this.webtransport = props?.webtransport;
		this.websocket = props?.websocket;

		this.#delay = this.delay.initial;

		this.closed = new Promise((resolve, reject) => {
			this.#closedResolve = resolve;
			this.#closedReject = reject;
		});

		this.signals.run(this.#connect.bind(this));
		this.signals.run(this.#runAnnounced.bind(this));
	}

	#connect(effect: Effect): void {
		effect.get(this.#tick);

		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const url = effect.get(this.url);
		if (!url) return;

		effect.set(this.status, "connecting", "disconnected");

		effect.spawn(async () => {
			try {
				const pending = connect(url, { websocket: this.websocket, webtransport: this.webtransport });

				const connection = await Promise.race([effect.cancel, pending]);
				if (!connection) {
					pending.then((conn) => conn.close()).catch(() => {});
					return;
				}

				effect.set(this.established, connection);
				effect.cleanup(() => connection.close());

				effect.set(this.status, "connected", "disconnected");

				this.#delay = this.delay.initial;
				this.#retryStart = undefined;

				await Promise.race([effect.cancel, connection.closed]);
			} catch (err) {
				console.warn("connection error:", err);

				this.#retryStart ??= performance.now();

				const timeout = this.delay.timeout ?? 300000;
				if (timeout > 0) {
					const elapsed = performance.now() - this.#retryStart;
					if (elapsed >= timeout) {
						console.warn("reconnect timed out");
						this.#closedReject(err instanceof Error ? err : new Error(String(err)));
						return;
					}
				}

				const tick = this.#tick.peek() + 1;
				effect.timer(() => this.#tick.update((prev) => Math.max(prev, tick)), this.#delay);

				this.#delay = Math.min(this.#delay * this.delay.multiplier, this.delay.max);
			}
		});
	}

	#runAnnounced(effect: Effect): void {
		this.#announced.set(new Set());

		const conn = effect.get(this.established);
		if (!conn) return;

		effect.cleanup(() => this.#announced.set(new Set()));

		// Cloudflare's relay does not yet support SUBSCRIBE_NAMESPACE, so skip
		// announce subscriptions entirely for those hosts.
		if (conn.url.hostname.endsWith("mediaoverquic.com")) {
			return;
		}

		const announced = conn.announced(Path.empty());
		effect.cleanup(() => announced.close());

		effect.spawn(async () => {
			try {
				for (;;) {
					const entry = await Promise.race([effect.cancel, announced.next()]);
					if (!entry) break;

					this.#announced.mutate((active) => {
						if (entry.active) {
							active.add(entry.path);
						} else {
							active.delete(entry.path);
						}
					});
				}
			} catch (err) {
				this.#announced.set(new Set());
				throw err;
			}
		});
	}

	close() {
		this.signals.close();
		this.#closedResolve();
	}
}
