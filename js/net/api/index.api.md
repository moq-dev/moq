# `@moq/net`

```ts
import { $ZodBranded } from "zod/v4/core";
import { Instant } from "@moq/clock";
import { Milli } from "@moq/clock";
import { ZodMiniBigInt } from "@zod/mini";
import { infer } from "@zod/mini";

export namespace Announce {
	export interface Announce {
		captures: Path.Pattern[] | undefined;
		prefix: Path.Valid;
		route: Origin.Route;
	}
	export class Consumer {
		[Symbol.asyncIterator](): AsyncGenerator<Announce.Event, void, undefined>;
		close(): void;
		get closed(): Signals.GetPromise<Error | null>;
		next(): Promise<Announce.Event | undefined>;
		get synced(): Signals.Getter<boolean>;
	}
	export type Event = {
		kind: "start" | "update" | "restart" | "end";
	} & Announce.Announce;
	export interface Options {
		covering?: boolean;
		hidden?: boolean;
	}
}

export namespace Auth {
	export interface Credential {
		close(): void;
		readonly closed: PromiseLike<Error | null>;
		readonly grant: Signals.Getter<Auth.Grant | undefined>;
	}
	export class Duplicate extends Error {
		constructor(message?: string);
	}
	export interface Grant {
		expires?: number;
		publish: Path.Patterns;
		subscribe: Path.Patterns;
	}
	export class Handle {
		constructor();
		abort(err?: Error): void;
		add(token: string | Uint8Array): Promise<Auth.Credential>;
		get grant(): Signals.Getter<Auth.Grant | undefined>;
	}
	export class Unsupported extends Error {
		constructor(message?: string);
		readonly code: SessionCode;
	}
}

export namespace Bandwidth {
	export class Allocator implements Bandwidth.Handle {
		constructor(estimate?: Signals.Getter<number | undefined>);
		close(): void;
		reserve(track: Track.Demand, max: number): Bandwidth.Reservation;
		static unlimited(): Bandwidth.Allocator;
	}
	export interface Handle {
		reserve(track: Track.Demand, max: number): Bandwidth.Reservation;
	}
	export class Reservation implements Disposable {
		[Symbol.dispose](): void;
		close(): void;
		peek(): number | undefined;
		get rate(): Signals.Getter<number | undefined>;
		update(max: number): void;
	}
}

export namespace Broadcast {
	export class Consumer {
		protected constructor(shared?: never);
		clone(): Broadcast.Consumer;
		close(): void;
		get closed(): Signals.GetPromise<null>;
		get epoch(): Epoch.Valid | undefined;
		get path(): Path.Valid;
		protected shareState(): never;
		track(name: string): Track.Consumer;
		tracks(): Broadcast.Tracks;
	}
	export class Producer {
		constructor();
		close(): void;
		get closed(): Signals.GetPromise<null>;
		consume(): Broadcast.Consumer;
		createTrack(name: string, info?: Partial<Track.Info>): Track.Producer;
		removeTrack(name: string): void;
		track(name: string): Track.Consumer;
	}
	export class Tracks {
		[Symbol.asyncIterator](): AsyncGenerator<Broadcast.TracksEvent, void, undefined>;
		close(): void;
		next(): Promise<Broadcast.TracksEvent | undefined>;
	}
	export type TracksEvent = {
		info: Track.Info;
		kind: "added";
		name: string;
	} | {
		kind: "synced";
	} | {
		kind: "removed";
		name: string;
	} | {
		kind: "finished";
		name: string;
	};
}

export namespace Connection {
	export interface AcceptProps {
		consume?: Origin.Producer;
		implementation?: string;
		publish?: Origin.Consumer;
		signal?: AbortSignal;
		timeout?: Milli;
		transport: WebTransport;
		url: URL;
		version?: number;
	}
	export type Backoff = {
		healthy?: Milli;
		initial?: Milli;
		max?: Milli;
		multiplier?: number;
		timeout?: Milli;
	};
	export interface CertificateHash {
		algorithm?: "sha-256";
		value: BufferSource | string;
	}
	export interface ConnectProps {
		auth?: Auth.Handle;
		consume?: Origin.Producer;
		implementation?: string;
		publish?: Origin.Consumer;
		signal?: AbortSignal;
		timeout?: Milli;
		tokens?: readonly string[];
		transport?: WebTransport;
		url: URL;
		versions?: readonly string[];
		websocket?: Connection.WebSocketProps;
		webtransport?: Connection.WebTransportProps;
	}
	export interface Established {
		abort(): void;
		announced(scope?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
		readonly auth: Auth.Handle;
		close(): Promise<void>;
		closed: Promise<Error | null>;
		readonly peerImplementation: Promise<string | undefined>;
		readonly probe: Signals.Getter<Connection.Probe>;
		stats(): Promise<Connection.Stats>;
		readonly transport: Connection.Transport;
		readonly url: URL;
		readonly version: string;
	}
	export interface Extensions {
		auth?: boolean;
		epoch?: boolean;
		solicit?: boolean;
	}
	export interface Goaway {
		handover?: Milli;
		redirect?: Connection.Redirect;
	}
	export interface Probe {
		estimatedRecvRate?: number;
		rtt?: Milli;
	}
	export interface Props {
		consume?: Origin.Producer;
		delay?: Connection.Backoff;
		enabled?: boolean | Signals.Signal<boolean>;
		goaway?: Connection.Goaway;
		publish?: Origin.Consumer;
		share?: boolean;
		signal?: never;
		timeout?: Milli;
		tokens?: readonly string[] | Signals.Signal<readonly string[]>;
		transport?: never;
		url?: URL | Signals.Signal<URL | undefined>;
		versions?: readonly string[] | Signals.Signal<readonly string[] | undefined>;
		websocket?: Connection.WebSocketProps;
		webtransport?: Connection.WebTransportProps;
	}
	export type Redirect = "follow" | "same-host" | "ignore";
	export interface Stats {
		bytesLost?: number;
		bytesReceived?: number;
		bytesSent?: number;
		estimatedSendRate?: number;
		packetsLost?: number;
		packetsReceived?: number;
		packetsSent?: number;
		rtt?: Milli;
	}
	export type Status = "connecting" | "connected" | "disconnected";
	export type Transport = "webtransport" | "websocket" | "webrtc";
	export interface WebSocketProps {
		delay?: Milli;
		enabled?: boolean;
		url?: URL;
	}
	export interface WebTransportProps extends Omit<WebTransportOptions, "serverCertificateHashes" | "protocols"> {
		serverCertificate?: string | BufferSource;
		serverCertificateHashes?: Connection.CertificateHash[];
	}
}

export class Connection {
	constructor(props?: Connection.Props);
	static readonly accept: ({ ... }: Connection.AcceptProps) => Promise<Connection.Established>;
	readonly allocator: Signals.Getter<Bandwidth.Handle | undefined>;
	announced(scope?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
	readonly auth: Signals.Getter<Auth.Handle | undefined>;
	static readonly certificateHash: (cert: string | BufferSource) => Promise<Uint8Array<ArrayBuffer>>;
	static readonly checkVersions: (versions: readonly string[]) => string[];
	close(abort?: Error): void;
	get closed(): Signals.GetPromise<Error | null>;
	static readonly connect: ({ ... }: Connection.ConnectProps) => Promise<Connection.Established>;
	enabled: Signals.Signal<boolean>;
	readonly error: Signals.Getter<Error | undefined>;
	static readonly isWebTransportSupported: () => boolean;
	readonly origin: Signals.Getter<Origin.Table | undefined>;
	readonly probe: Signals.Getter<Connection.Probe | undefined>;
	stats(): Promise<Connection.Stats | undefined>;
	readonly status: Signals.Getter<Connection.Status>;
	readonly transport: Signals.Getter<Connection.Transport | undefined>;
	url: Signals.Signal<URL | undefined>;
	versions: Signals.Signal<readonly string[] | undefined>;
}

export namespace Epoch {
	export type Valid = string & {
		readonly _brand: "epoch";
	};
	export function mint(): Epoch.Valid;
	export function parse(text: string): Epoch.Valid;
}

export namespace Error {
	export class ConnectTimeout extends Error {
		constructor(timeout: Milli);
		readonly timeout: Milli;
	}
	export class FrameTooLarge extends Error.Stream {
		constructor(options?: {
			cause?: unknown;
		});
	}
	export class GroupTooLarge extends Error.Stream {
		constructor(options?: {
			cause?: unknown;
		});
	}
	export class NoStreamCredit extends Error {
		constructor(options?: ErrorOptions);
	}
	export class NotFound extends Error.Stream {
		constructor(what: string, options?: {
			cause?: unknown;
		});
	}
	export class ProtocolViolation extends Error {
		constructor(message: string, options?: {
			cause?: unknown;
			code?: SessionCode;
		});
		readonly code: SessionCode;
	}
	export class RefusedRedirect extends Error {
		constructor(reason: string);
	}
	export class Session extends Error {
		constructor(code: SessionCode, options?: {
			cause?: unknown;
			reason?: string;
		});
		readonly code: SessionCode;
	}
	export class Stream extends Error {
		constructor(code: StreamCode, options?: Error.StreamOptions);
		readonly code: StreamCode;
	}
	export interface StreamOptions {
		cause?: unknown;
		message?: string;
		reason?: string;
	}
	export class TimestampMismatch extends Error.Stream {
		constructor(options?: {
			cause?: unknown;
		});
	}
	export class TooFarBehind extends Error.Stream {
		constructor(options?: {
			cause?: unknown;
		});
	}
}

export namespace Group {
	export class Consumer {
		get arrived(): Instant;
		close(): void;
		get closed(): Signals.GetPromise<Error | null>;
		get done(): boolean;
		get frameCount(): number;
		get isClosed(): boolean;
		readBool(): Promise<boolean | undefined>;
		readFrame(): Promise<Group.Frame | undefined>;
		readFrameSequence(options?: Group.ReadOptions): Promise<{
			sequence: number;
		} & Group.Frame | undefined>;
		readJson(): Promise<unknown | undefined>;
		readString(): Promise<string | undefined>;
		readable(): Promise<void>;
		readonly sequence: number;
		tryReadFrame(): Group.Frame | undefined;
		tryReadFrameSequence(): {
			sequence: number;
		} & Group.Frame | undefined;
	}
	export interface Frame {
		arrived?: Instant;
		payload: Uint8Array;
		timestamp?: Time.Timestamp;
	}
	export interface Info {
		sequence: number;
	}
	export const MAX_GROUP_CACHE_BYTES: number;
	export const MAX_GROUP_FRAMES: 8192;
	export class Producer {
		constructor(sequence: number);
		abort(error: Error): void;
		get closed(): Signals.GetPromise<Error | null>;
		consume(): Group.Consumer;
		finish(): void;
		get isClosed(): boolean;
		readonly sequence: number;
		writeFrame(frame: Group.Frame): void;
	}
	export interface ReadOptions {
		from?: number;
	}
}

export const IMPLEMENTATION: string;

export namespace Origin {
	export class Consumer {
		announced(scope?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
		get closed(): Signals.GetPromise<Error | null>;
		get discovery(): Signals.Getter<boolean | undefined>;
		follow(path: Path.Valid, options?: Origin.RequestOptions): Origin.Requesting;
		request(path: Path.Valid, options?: Origin.RequestOptions): Origin.Requesting;
	}
	export type Cost = bigint;
	export type Hop = infer<typeof Origin.HopSchema>;
	export const HopSchema: $ZodBranded<ZodMiniBigInt<bigint>, "Hop", "out">;
	export class Prepared {
		[Symbol.dispose](): void;
		readonly broadcast: Broadcast.Producer;
		close(): void;
		publish(): Origin.Publication;
	}
	export class Producer implements Origin.Table {
		constructor();
		announced(scope?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
		close(abort?: Error): void;
		get closed(): Signals.GetPromise<Error | null>;
		consume(): Origin.Consumer;
		createBroadcast(path: Path.Valid): Origin.Publication;
		get discovery(): Signals.Getter<boolean | undefined>;
		follow(path: Path.Valid, options?: Origin.RequestOptions): Origin.Requesting;
		request(path: Path.Valid, options?: Origin.RequestOptions): Origin.Requesting;
		requests(prefix: Path.Valid): Origin.Requests;
		scope(root: Path.Valid, patterns: Path.Patterns): Origin.Producer;
	}
	export class Publication {
		announce(route?: Origin.Route | {
			auth?: Origin.Route["auth"];
			cost?: Origin.Route["cost"];
			epoch?: Origin.Route["epoch"];
			hops?: Origin.Route["hops"];
		}): void;
		readonly broadcast: Broadcast.Producer;
		get route(): Origin.Route | undefined;
		unannounce(): void;
	}
	export class Request {
		readonly path: Path.Valid;
		prepare(epoch: Epoch.Valid): Origin.Prepared;
		reject(err: Error): void;
	}
	export interface RequestOptions {
		epoch?: Epoch.Valid;
	}
	export class Requesting {
		readonly active: Signals.Getter<Broadcast.Consumer | undefined>;
		close(): void;
		readonly closed: Signals.GetPromise<Error | null>;
		readonly path: Path.Valid;
	}
	export class Requests {
		close(): void;
		readonly prefix: Path.Valid;
		requested(): AsyncIterableIterator<Origin.Request>;
		get route(): Origin.Route;
	}
	export interface Route {
		auth?: Auth.Credential;
		cost: Origin.Cost;
		epoch?: Epoch.Valid;
		hops: Origin.Hop[];
	}
	export interface Table {
		announced(scope?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
		readonly closed: Signals.GetPromise<Error | null>;
		createBroadcast(path: Path.Valid): Origin.Publication;
		readonly discovery: Signals.Getter<boolean | undefined>;
		follow(path: Path.Valid, options?: Origin.RequestOptions): Origin.Requesting;
		request(path: Path.Valid, options?: Origin.RequestOptions): Origin.Requesting;
		requests(prefix: Path.Valid): Origin.Requests;
	}
	export function isAnonymous(route: Origin.Route): boolean;
}

export namespace Path {
	export { InvalidPattern } from "@moq/pattern";
	export const MAX_BYTES: 4096;
	export const MAX_PARTS: 32;
	export { Pattern } from "@moq/pattern";
	export { Patterns } from "@moq/pattern";
	export type Relative = string & {
		__brand: "Relative";
	};
	export { Segment } from "@moq/pattern";
	export { Specificity } from "@moq/pattern";
	export type Valid = string & {
		__brand: "Name";
	};
	export { compareSpecificity } from "@moq/pattern";
	export function decode(raw: string): Path.Valid;
	export function empty(): Path.Valid;
	export function encode(path: Path.Valid): Path.Valid;
	export function from(...paths: string[]): Path.Valid;
	export function hasPrefix(prefix: Path.Valid, path: Path.Valid): boolean;
	export function join(path: Path.Valid, other: Path.Valid): Path.Valid;
	export function normalizeRelative(rel: string): Path.Relative;
	export function parts(path: Path.Valid): string[];
	export function relative(target: Path.Valid, base: Path.Valid): Path.Relative | undefined;
	export function resolve(base: Path.Valid, rel: Path.Relative): Path.Valid;
	export function stripPrefix(prefix: Path.Valid, path: Path.Valid): Path.Valid | null;
	export function tryResolve(base: Path.Valid, rel: Path.Relative): Path.Valid | undefined;
}

export const SessionCode: ((code: number) => SessionCode) & {
	readonly AuthTokenCacheOverflow: SessionCode;
	readonly Cancel: SessionCode;
	readonly Expired: SessionCode;
	readonly GoawayTimeout: SessionCode;
	readonly Internal: SessionCode;
	readonly InvalidAuthority: SessionCode;
	readonly InvalidPath: SessionCode;
	readonly InvalidRequestId: SessionCode;
	readonly KeyValueFormatting: SessionCode;
	readonly MalformedAuthToken: SessionCode;
	readonly ProtocolViolation: SessionCode;
	readonly Timeout: SessionCode;
	readonly TooManyRequestUpdates: SessionCode;
	readonly TooManyRequests: SessionCode;
	readonly Unauthorized: SessionCode;
	readonly UnknownAuthTokenAlias: SessionCode;
	readonly Unsupported: SessionCode;
	readonly Version: SessionCode;
};

export type SessionCode = number & {
	readonly [SESSION_CODE]: true;
};

export * as Signals from "@moq/signals";

export const StreamCode: ((code: number) => StreamCode) & {
	readonly Cancel: StreamCode;
	readonly ControlTimeout: StreamCode;
	readonly DeliveryTimeout: StreamCode;
	readonly Evicted: StreamCode;
	readonly FrameTooLarge: StreamCode;
	readonly GoingAway: StreamCode;
	readonly GroupTooLarge: StreamCode;
	readonly Internal: StreamCode;
	readonly MalformedTrack: StreamCode;
	readonly NotFound: StreamCode;
	readonly Old: StreamCode;
	readonly SessionClosed: StreamCode;
	readonly TimestampMismatch: StreamCode;
	readonly TooFarBehind: StreamCode;
	readonly Unauthorized: StreamCode;
	readonly Unroutable: StreamCode;
};

export type StreamCode = number & {
	readonly [STREAM_CODE]: true;
};

export namespace Time {
	export const Timescale: ((unitsPerSecond: number) => Time.Timescale) & {
		MICRO: Time.Timescale;
		MILLI: Time.Timescale;
		NANO: Time.Timescale;
		SECOND: Time.Timescale;
	};
	export type Timescale = number & {
		readonly _brand: "timescale";
	};
	export class Timestamp {
		constructor(value: number, scale: Time.Timescale);
		as(scale: Time.Timescale): number;
		asMicros(): number;
		asMillis(): number;
		static fromMicros(us: number): Time.Timestamp;
		static fromMillis(ms: number): Time.Timestamp;
		isBefore(other: Time.Timestamp): boolean;
		static now(): Time.Timestamp;
		readonly scale: Time.Timescale;
		readonly value: number;
	}
}

export interface Timed<T> {
	at?: Time.Timestamp;
	value: T;
}

export namespace Track {
	export type Bound = {
		excluded?: never;
		included: number;
	} | {
		excluded: number;
		included?: never;
	};
	export class Consumer {
		fetchGroup(sequence: number, options?: Track.FetchGroupOptions): Promise<Group.Consumer>;
		info(): Promise<Track.Info>;
		readonly name: string;
		subscribe(options?: Track.Subscription): Track.Subscriber;
	}
	export interface Datagram {
		payload: Uint8Array;
		sequence: number;
		timestamp?: Time.Timestamp;
	}
	export class Demand {
		get closed(): Signals.GetPromise<Error | null>;
		readonly name: string;
		get wanted(): Signals.Getter<number | undefined>;
	}
	export interface FetchGroupOptions {
		maxDelay?: Milli;
		priority?: number;
		signal?: AbortSignal;
	}
	export interface Groups {
		end?: Track.Bound;
		start?: Track.Bound;
	}
	export interface Info {
		compress?: boolean;
		maxAge?: Milli;
		priority: number;
		refresh?: number;
		timescale?: Time.Timescale | null;
	}
	export type Order = "ascending" | "descending";
	export class Ordered {
		close(abort?: Error): void;
		get closed(): Signals.GetPromise<Error | null>;
		finished(): Promise<Track.Position>;
		info(): Promise<Track.Info>;
		largest(): Promise<Track.Position | undefined>;
		readonly name: string;
		nextGroup(): Promise<Group.Consumer | undefined>;
		readBool(): Promise<boolean | undefined>;
		readFrame(): Promise<{
			frame: number;
			group: number;
		} & Group.Frame | undefined>;
		readJson(): Promise<unknown | undefined>;
		readString(): Promise<string | undefined>;
		recvDatagram(): Promise<Track.Datagram | undefined>;
		get subscription(): Signals.Getter<Track.Subscription | undefined>;
		tryNextGroup(): Group.Consumer | undefined;
		update(options: Track.Subscription): void;
		withRange(groups: Track.Groups): this;
	}
	export interface Position {
		frame: number;
		group: number;
	}
	export class Producer {
		constructor(name: string);
		abort(error: Error): void;
		accept(info?: Partial<Track.Info>): this;
		appendDatagram(timestamp: Time.Timestamp | undefined, payload: Uint8Array): number;
		appendGroup(): Group.Producer;
		close(): void;
		get closed(): Signals.GetPromise<Error | null>;
		createGroup(sequence: number): Group.Producer;
		demand(): Track.Demand;
		finish(): void;
		finishAt(end: Track.Position): void;
		info(): Promise<Track.Info>;
		insertDatagram(sequence: number, timestamp: Time.Timestamp | undefined, payload: Uint8Array): void;
		largest(): Track.Position | undefined;
		readonly name: string;
		get priority(): number;
		subscribe(options?: Track.Subscription): Track.Subscriber;
		get subscription(): Signals.Getter<Track.Subscription | undefined>;
		writeFrame(frame: Group.Frame): void;
	}
	export class Request {
		accept(info?: Partial<Track.Info>): Track.Producer;
		demand(): Track.Demand;
		readonly name: string;
		get priority(): number;
		reject(err: Error): void;
		get subscription(): Readonly<Track.Subscription>;
	}
	export class Subscriber {
		close(abort?: Error): void;
		get closed(): Signals.GetPromise<Error | null>;
		finished(): Promise<Track.Position>;
		info(): Promise<Track.Info>;
		largest(): Promise<Track.Position | undefined>;
		readonly name: string;
		ordered(): Track.Ordered;
		recvDatagram(): Promise<Track.Datagram | undefined>;
		recvGroup(): Promise<Group.Consumer | undefined>;
		get subscription(): Signals.Getter<Track.Subscription | undefined>;
		tryRecvGroup(): Group.Consumer | undefined;
		update(options: Track.Subscription): void;
		withRange(groups: Track.Groups): this;
	}
	export const Subscription: {
		default(): Track.Subscription;
	};
	export interface Subscription {
		groups?: Track.Groups;
		maxDelay?: Milli;
		order?: Track.Order;
		priority?: number;
	}
	export function infoDefaults(info?: Partial<Track.Info>): Track.Info;
}

export namespace Varint {
	export const MAX_U14: number;
	export const MAX_U30: number;
	export const MAX_U53: number;
	export const MAX_U6: number;
	export function decode(buf: Uint8Array): [number, Uint8Array];
	export function decodeBigInt(buf: Uint8Array): [bigint, Uint8Array];
	export function decodeLeadingOnes(buf: Uint8Array): [bigint, Uint8Array];
	export function encode(v: number | bigint): Uint8Array;
	export function encodeLeadingOnes(v: number | bigint): Uint8Array;
	export function encodeLeadingOnesTo(dst: ArrayBuffer, v: number | bigint): Uint8Array;
	export function encodeTo(dst: ArrayBuffer, v: number | bigint): Uint8Array;
	export function size(v: number): number;
}
```
