# `@moq/qmux`

```ts
export interface AcceptOptions extends Pick<SessionOptions, "config" | "sendBufferSize"> {
	protocol?: string;
}

export interface Config {
	idleTimeout?: number | null;
	maxDatagramFrameSize?: bigint | null;
	maxRecordSize?: bigint;
	maxStreamsBidi?: bigint;
	maxStreamsUni?: bigint;
	receiveWindow?: bigint;
	streamReceiveWindow?: bigint;
}

export interface InstallOptions {
	requireProtocol?: boolean;
	versions?: Record<string, Version | Version[] | null>;
}

export type SendStreamOptions = WebTransportSendStreamOptions & {
	waitUntilAvailable?: boolean;
};

export class SessionError extends Error implements Pick<WebTransportError, "source" | "streamErrorCode"> {
	constructor(message: string, options?: {
		cause?: unknown;
	});
	readonly source: "session";
	readonly streamErrorCode: null;
}

export interface SessionOptions extends WebTransportOptions {
	config?: Config;
	protocols?: string[];
	requireProtocol?: boolean;
	sendBufferSize?: number;
	versions?: Record<string, Version | Version[] | null>;
}

export class StreamError extends Error implements Pick<WebTransportError, "source" | "streamErrorCode"> {
	constructor(code: number, message?: string);
	readonly source: "stream";
	readonly streamErrorCode: number;
}

export type Version = "qmux-00" | "qmux-01" | "qmux-02";

export interface WebSocketLike {
	binaryType: string;
	readonly bufferedAmount: number;
	close(code?: number, reason?: string): void;
	readonly extensions: string;
	onclose: ((ev: {
		code?: number;
		reason?: string;
	}) => void) | null;
	onerror: ((ev: unknown) => void) | null;
	onmessage: ((ev: {
		data: unknown;
	}) => void) | null;
	onopen: ((ev: unknown) => void) | null;
	readonly protocol: string;
	readonly readyState: number;
	send(data: string | ArrayBufferLike | ArrayBufferView): void;
	readonly url?: string;
}

export interface WebSocketStreamCloseEvent {
	closeCode: number;
	reason: string;
}

export interface WebSocketStreamCloseInfo {
	closeCode?: number;
	reason?: string;
}

export type WebSocketStreamData = Uint8Array | string;

export interface WebSocketStreamLike {
	close(closeInfo?: WebSocketStreamCloseInfo): void;
	readonly closed: Promise<WebSocketStreamCloseEvent>;
	readonly opened: Promise<WebSocketStreamOpenEvent>;
	setHighWaterMark?(bytes: number): void;
	readonly url: string;
}

export interface WebSocketStreamOpenEvent {
	extensions: string;
	protocol: string;
	readable: ReadableStream<WebSocketStreamData>;
	writable: WritableStream<WebSocketStreamData>;
}

export default class implements WebTransport {
	constructor(source: string | URL | WebSocketLike | WebSocketStreamLike, options?: SessionOptions);
	static accept(socket: WebSocketLike | WebSocketStreamLike, options?: AcceptOptions): import("@moq/qmux").default;
	close(info?: {
		closeCode?: number;
		reason?: string;
	}): void;
	readonly closed: Promise<WebTransportCloseInfo>;
	get congestionControl(): WebTransportCongestionControl;
	createBidirectionalStream(options?: SendStreamOptions): Promise<WebTransportBidirectionalStream>;
	createUnidirectionalStream(options?: SendStreamOptions): Promise<WritableStream<Uint8Array>>;
	get datagrams(): WebTransportDatagramDuplexStream;
	getStats(): Promise<{
		datagrams: {};
	}>;
	readonly incomingBidirectionalStreams: ReadableStream<WebTransportBidirectionalStream>;
	readonly incomingUnidirectionalStreams: ReadableStream<ReadableStream<Uint8Array>>;
	get protocol(): string;
	readonly ready: Promise<void>;
	get reliability(): "reliable-only";
	setSendBufferSize(bytes: number): void;
}

export function install(options?: InstallOptions): boolean;

export function selectSubprotocol(offered: string | readonly string[] | null | undefined, options?: Pick<SessionOptions, "protocols" | "versions" | "requireProtocol">): string | undefined;
```
