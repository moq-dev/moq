# `@moq/net`

```ts
export namespace Announce {
	export interface Announce {
		captures: Path.Pattern[] | undefined;
		prefix: Path.Valid;
		route: Origin.RouteInfo;
	}
	export class Consumer {
		[Symbol.asyncIterator](): AsyncGenerator<Announce.Event, void, undefined>;
		close(): void;
		get closed(): Signals.GetPromise<Error | null>;
		next(): Promise<Announce.Event | undefined>;
		get synced(): Signals.Getter<boolean>;
	}
	export type Event = {
		kind: "present" | "unavailable" | "available" | "absent" | "offline";
	} & Announce.Announce;
	export interface Options {
		covering?: boolean;
		hidden?: boolean;
	}
}

export namespace Auth {
	export interface Credential {
		readonly answered: PromiseLike<void>;
		close(): void;
		readonly closed: PromiseLike<Error | null>;
		readonly grant: Signals.Getter<Auth.Grant | undefined>;
	}
	export class Duplicate extends Error {
		constructor(message?: string);
	}
	export interface Grant {
		expires?: Clock.Wall;
		publish: Path.Patterns;
		subscribe: Path.Patterns;
	}
	export class Handle {
		constructor();
		abort(err?: Error): void;
		add(token: string | Uint8Array): Auth.Credential;
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
	export class Viewer implements Disposable {
		[Symbol.dispose](): void;
		close(): void;
		peek(): number | undefined;
		get rate(): Signals.Getter<number | undefined>;
		update(max: number): void;
	}
	export class Viewers {
		constructor(estimate?: Signals.Getter<number | undefined>);
		static readonly MAX_TARGET: number;
		close(): void;
		setTarget(target: number | undefined): void;
		readonly target: Signals.Getter<number>;
		watch(max: number): Bandwidth.Viewer;
	}
}

export namespace Broadcast {
	export class Consumer {
		protected constructor(shared?: never);
		clone(): Broadcast.Consumer;
		close(): void;
		get closed(): Signals.GetPromise<null>;
		datagrams(name: string): Datagram.Consumer;
		get instance(): Instance.Valid | undefined;
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
		createDatagrams(name: string, info?: Partial<Datagram.Info>): Datagram.Producer;
		createTrack(name: string, info?: Track.InfoInit): Track.Producer;
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

export * as Clock from "@moq/clock";

export namespace Connection {
	export interface AcceptProps {
		consume?: Origin.Producer;
		implementation?: string;
		publish?: Origin.Consumer;
		signal?: AbortSignal;
		timeout?: Clock.Duration;
		transport: WebTransport;
		url: URL;
		version?: number;
	}
	export type Backoff = {
		healthy?: Clock.Duration;
		initial?: Clock.Duration;
		max?: Clock.Duration | null;
		multiplier?: number;
		timeout?: Clock.Duration | null;
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
		timeout?: Clock.Duration;
		tokens?: readonly string[];
		transport?: WebTransport;
		url: URL;
		versions?: readonly string[];
		viewers?: Bandwidth.Viewers;
		websocket?: Connection.WebSocketProps;
		webtransport?: Connection.WebTransportProps;
	}
	export interface Drain {
		readonly timeout?: Clock.Duration;
		readonly uri: string;
	}
	export interface Goaway {
		handover?: Clock.Duration;
		redirect?: Connection.Redirect;
	}
	export interface Probe {
		estimatedRecvRate?: number;
		rtt?: Clock.Duration;
	}
	export interface Props {
		consume?: Origin.Producer;
		delay?: Connection.Backoff;
		enabled?: boolean | Signals.Signal<boolean>;
		goaway?: Connection.Goaway;
		publish?: Origin.Consumer;
		share?: boolean;
		signal?: never;
		timeout?: Clock.Duration;
		tokens?: readonly string[] | Signals.Signal<readonly string[]>;
		transport?: never;
		url?: URL | Signals.Signal<URL | undefined>;
		versions?: readonly string[] | Signals.Signal<readonly string[] | undefined>;
		websocket?: Connection.WebSocketProps;
		webtransport?: Connection.WebTransportProps;
	}
	export type Redirect = "follow" | "same-host" | "ignore";
	export interface Session {
		abort(): void;
		announced(scope?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
		readonly auth: Auth.Handle;
		close(): Promise<void>;
		closed: Promise<Error | null>;
		readonly draining: Signals.GetPromise<Connection.Drain>;
		readonly peerImplementation: Promise<string | undefined>;
		readonly probe: Signals.Getter<Connection.Probe>;
		stats(): Promise<Connection.Stats>;
		readonly transport: Connection.Transport;
		readonly url: URL;
		readonly version: string;
	}
	export interface Stats {
		bytesLost?: number;
		bytesReceived?: number;
		bytesSent?: number;
		datagramsUnrouted?: number;
		estimatedSendRate?: number;
		packetsLost?: number;
		packetsReceived?: number;
		packetsSent?: number;
		rtt?: Clock.Duration;
	}
	export type Status = "connecting" | "connected" | "disconnected";
	export type Transport = "webtransport" | "websocket" | "webrtc";
	export interface WebSocketProps {
		delay?: Clock.Duration;
		enabled?: boolean;
		maxStreams?: number;
		receiveWindow?: number;
		streamReceiveWindow?: number;
		url?: URL;
	}
	export interface WebTransportProps extends Omit<WebTransportOptions, "serverCertificateHashes" | "protocols"> {
		serverCertificate?: string | BufferSource;
		serverCertificateHashes?: Connection.CertificateHash[];
	}
}

export class Connection {
	constructor(props?: Connection.Props);
	static readonly accept: ({ ... }: Connection.AcceptProps) => Promise<Connection.Session>;
	readonly allocator: Signals.Getter<Bandwidth.Handle | undefined>;
	announced(scope?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
	readonly auth: Signals.Getter<Auth.Handle | undefined>;
	static readonly certificateHash: (cert: string | BufferSource) => Promise<Uint8Array<ArrayBuffer>>;
	static readonly checkVersions: (versions: readonly string[]) => string[];
	close(abort?: Error): void;
	get closed(): Signals.GetPromise<Error | null>;
	static readonly connect: (props: Connection.ConnectProps) => Promise<Connection.Session>;
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
	readonly viewers: Signals.Getter<Pick<Bandwidth.Viewers, "watch" | "setTarget" | "target"> | undefined>;
}

export namespace Datagram {
	export class Consumer {
		clone(): Datagram.Consumer;
		close(error?: Error): void;
		info(): Promise<Datagram.Info>;
		readonly name: string;
		get priority(): number;
		recv(): Promise<Datagram.Datagram | undefined>;
		setPriority(value: number): void;
	}
	export interface Datagram {
		payload: Uint8Array;
		sequence: bigint;
		timestamp?: Time.Timestamp;
	}
	export interface Info {
		priority: number;
		timescale: Time.Timescale | null;
	}
	export class Producer {
		constructor(name: string);
		abort(error: Error): void;
		accept(properties?: Partial<Datagram.Info>): this;
		append(timestamp: Time.Timestamp | undefined, payload: Uint8Array): bigint;
		get closed(): Signals.GetPromise<Error | null>;
		consume(): Datagram.Consumer;
		finish(): void;
		info(): Promise<Datagram.Info>;
		insert(datagram: Datagram.Datagram): void;
		readonly name: string;
		get priority(): Signals.Getter<number | undefined>;
	}
	export function rescale(info: {
		timescale?: Time.Timescale | null;
	}, timestamp: Time.Timestamp | undefined): Time.Timestamp | undefined;
}

export namespace Error {
	export class ConnectTimeout extends Error {
		constructor(timeout: Clock.Duration);
		readonly timeout: Clock.Duration;
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
	export class Skipped extends Error {
		constructor(sequence: bigint);
		readonly sequence: bigint;
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

export namespace Frame {
	export class Consumer {
		get arrived(): Clock.Instant | undefined;
		readAll(): Promise<Uint8Array>;
		readChunk(): Promise<Uint8Array | undefined>;
		readonly sequence: number;
		readonly size: number;
		readonly timestamp: Time.Timestamp | undefined;
	}
	export interface Info {
		size: number;
		timestamp?: Time.Timestamp;
	}
	export class Producer {
		abort(error: Error): void;
		finish(): void;
		get remaining(): number;
		readonly size: number;
		readonly timestamp: Time.Timestamp | undefined;
		write(chunk: Uint8Array): void;
	}
}

export namespace Group {
	export class Consumer {
		get arrived(): Clock.Instant;
		close(): void;
		get closed(): Signals.GetPromise<Error | null>;
		get done(): boolean;
		get frameCount(): number;
		get isClosed(): boolean;
		nextFrame(): Promise<Frame.Consumer | undefined>;
		readFrame(): Promise<Group.Frame | undefined>;
		readFrameSequence(options?: Group.ReadOptions): Promise<{
			sequence: number;
		} & Group.Frame | undefined>;
		readJson(): Promise<unknown | undefined>;
		readString(): Promise<string | undefined>;
		readable(): Promise<void>;
		readonly sequence: bigint;
		tryReadFrame(): Group.Frame | undefined;
		tryReadFrameSequence(): {
			sequence: number;
		} & Group.Frame | undefined;
	}
	export interface Finished {
		readonly closed: Signals.GetPromise<Error | null>;
		consume(): Group.Consumer;
		readonly sequence: bigint;
	}
	export interface Frame {
		arrived?: Clock.Instant;
		payload: Uint8Array;
		timestamp?: Time.Timestamp;
	}
	export interface Info {
		sequence: bigint;
	}
	export class Producer {
		constructor(sequence: bigint);
		abort(error: Error): void;
		get closed(): Signals.GetPromise<Error | null>;
		consume(): Group.Consumer;
		createFrame(info: Frame.Info): Frame.Producer;
		finish(): Group.Finished;
		get isClosed(): boolean;
		readonly sequence: bigint;
		writeFrame(frame: Group.Frame): void;
	}
	export interface ReadOptions {
		from?: number;
	}
}

export namespace Instance {
	export interface Minted {
		get text(): string;
		toString(): string;
	}
	export type Named = string & {
		readonly _brand: "instance";
	};
	export type Valid = Instance.Named | Instance.Minted;
	export function isMinted(instance: Instance.Valid): instance is Instance.Minted;
	export function parse(text: string): Instance.Named;
	export function random(): Instance.Named;
	export function text(instance: Instance.Valid): string;
}

export namespace Origin {
	export class Claim {
		[Symbol.dispose](): void;
		announce(): void;
		get announced(): boolean;
		close(): void;
		get instance(): Instance.Valid;
		requested(): AsyncIterableIterator<Origin.Request>;
		setAuth(auth?: Auth.Credential): void;
		setCost(cost: Origin.Cost): void;
		withdraw(): void;
	}
	export class Consumer implements Origin.Reader {
		announced(scope?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
		get closed(): Signals.GetPromise<Error | null>;
		get discovery(): Signals.Getter<boolean | undefined>;
		follow(path: Path.Valid, options?: Origin.FollowOptions): Origin.Following;
		request(path: Path.Valid, options?: Origin.RequestOptions): Origin.Requesting;
	}
	export type Cost = bigint;
	export interface FollowOptions {
		instance?: Instance.Valid;
	}
	export class Following {
		readonly active: Signals.Getter<Broadcast.Consumer | undefined>;
		close(): void;
		readonly closed: Signals.GetPromise<Error | null>;
		readonly path: Path.Valid;
	}
	export class Producer implements Origin.Table {
		constructor();
		announced(scope?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
		claim(prefix: Path.Valid, options?: Origin.RouteOptions): Origin.Claim;
		close(abort?: Error): void;
		get closed(): Signals.GetPromise<Error | null>;
		consume(): Origin.Consumer;
		get discovery(): Signals.Getter<boolean | undefined>;
		follow(path: Path.Valid, options?: Origin.FollowOptions): Origin.Following;
		request(path: Path.Valid, options?: Origin.RequestOptions): Origin.Requesting;
		route(path: Path.Valid, options?: Origin.RouteOptions): Origin.Route;
		scope(root: Path.Valid, patterns: Path.Patterns): Origin.Producer;
	}
	export interface Reader {
		announced(scope?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
		readonly closed: Signals.GetPromise<Error | null>;
		readonly discovery: Signals.Getter<boolean | undefined>;
		follow(path: Path.Valid, options?: Origin.FollowOptions): Origin.Following;
		request(path: Path.Valid, options?: Origin.RequestOptions): Origin.Requesting;
	}
	export class Request {
		readonly path: Path.Valid;
		reject(err: Error): void;
		route(options?: Origin.RouteOptions): Origin.Route;
		serve(source: Broadcast.Producer | Broadcast.Consumer): void;
	}
	export interface RequestOptions {
		instance?: Instance.Valid;
	}
	export class Requesting {
		readonly active: Signals.Getter<Broadcast.Consumer | undefined>;
		close(): void;
		readonly closed: Signals.GetPromise<Error | null>;
		readonly path: Path.Valid;
	}
	export class Route {
		[Symbol.dispose](): void;
		announce(): void;
		get announced(): boolean;
		readonly broadcast: Broadcast.Producer;
		close(): void;
		get instance(): Instance.Valid;
		setAuth(auth?: Auth.Credential): void;
		setCost(cost: Origin.Cost): void;
		withdraw(): void;
	}
	export interface RouteInfo {
		readonly available: boolean;
		readonly cost: Origin.Cost;
		readonly instance?: Instance.Valid;
		readonly rtt?: Clock.Duration;
	}
	export interface RouteOptions {
		auth?: Auth.Credential;
		cost?: Origin.Cost;
		hidden?: boolean;
		instance?: {
			epoch: bigint;
			id: Instance.Valid;
		};
	}
	export interface Table extends Origin.Reader {
		claim(prefix: Path.Valid, options?: Origin.RouteOptions): Origin.Claim;
		route(path: Path.Valid, options?: Origin.RouteOptions): Origin.Route;
	}
}

export namespace Path {
	export { InvalidPattern } from "@moq/pattern";
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

export namespace Socket {
	export class Endpoint {
		constructor(origin: Origin.Producer, prefix: Path.Valid, id: string);
		acceptTcp(options?: Socket.Options): Promise<Socket.Incoming<Socket.TcpStream>>;
		acceptTrack(options?: Socket.Options): Promise<Socket.Incoming<Socket.TrackStream>>;
		close(): void;
		get closed(): Signals.GetPromise<Error | null>;
		dialTcp(peer: string, options?: Socket.Options): Promise<Socket.TcpStream>;
		dialTrack(peer: string, info?: Track.InfoInit, options?: Socket.Options): Promise<Socket.TrackStream>;
		readonly id: string;
		readonly prefix: Path.Valid;
	}
	export class Incoming<S extends Socket.TcpStream | Socket.TrackStream> {
		accept(...args: S extends Socket.TcpStream ? [options?: Socket.Options] : [info?: Track.InfoInit, options?: Socket.Options]): Promise<S>;
		readonly peer: string;
		reject(): void;
		readonly session: string;
	}
	export interface Options {
		signal?: AbortSignal;
	}
	export class TcpStream {
		readonly consumer: Group.Consumer;
		readonly peer: string;
		readonly producer: Group.Producer;
		readonly session: string;
	}
	export class TrackStream {
		readonly consumer: Track.Subscriber;
		readonly peer: string;
		readonly producer: Track.Producer;
		readonly session: string;
	}
	export function isEndpoint(path: Path.Valid): boolean;
}

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
	readonly Yielded: StreamCode;
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
	export type Anchor = {
		absolute: bigint;
		relative?: never;
		time?: never;
	} | {
		absolute?: never;
		relative: bigint;
		time?: never;
	} | {
		absolute?: never;
		relative?: never;
		time: Time.Timestamp;
	};
	export type Bound = {
		excluded?: never;
		included: bigint;
	} | {
		excluded: bigint;
		included?: never;
	};
	export type Compression = "none" | "deflate";
	export class Consumer {
		fetchGroup(sequence: bigint, options?: Track.FetchGroupOptions): Promise<Group.Consumer>;
		info(options?: Track.InfoOptions): Promise<Track.Info>;
		readonly name: string;
		subscribe(options?: Track.Subscription): Track.Subscriber;
	}
	export interface Delay {
		max?: Clock.Duration | null;
		min?: Clock.Duration | null;
	}
	export class Demand {
		get closed(): Signals.GetPromise<Error | null>;
		readonly name: string;
		get wanted(): Signals.Getter<bigint | undefined>;
	}
	export interface FetchGroupOptions {
		maxDelay?: Clock.Duration;
		priority?: number;
		signal?: AbortSignal;
	}
	export class Fill {
		[Symbol.dispose](): void;
		accept(sequence: bigint, info?: Track.Info): Group.Producer;
		close(): void;
		dropGroups(start: bigint, end: bigint): void;
		readonly end: bigint;
		isWanted(sequence: bigint): boolean;
		get maxDelay(): Clock.Duration | null;
		get order(): Track.Order;
		get priority(): number;
		refuseGroups(start: bigint, end: bigint, error: Error): void;
		readonly start: bigint;
		unwanted(sequence: bigint): Promise<void>;
		wanted(): bigint | undefined;
		wantedChanged(): Promise<bigint | undefined>;
	}
	export interface Finished {
		abort(error: Error): void;
		close(): void;
		readonly closed: Signals.GetPromise<Error | null>;
		createGroup(sequence: bigint): Group.Producer;
		info(): Promise<Track.Info>;
		readonly name: string;
		requests(): Track.Requests;
		subscribe(options?: Track.Subscription): Track.Subscriber;
	}
	export interface Groups {
		end?: Track.Bound;
		start?: Track.Bound;
	}
	export interface Info {
		compression: Track.Compression;
		maxAge: Clock.Duration | null;
		priority: number;
		refresh: number | null;
		timescale: Time.Timescale | null;
	}
	export interface InfoInit extends Partial<Track.Info> {
		maxAge?: Clock.Duration | null;
	}
	export interface InfoOptions {
		signal?: AbortSignal;
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
		readFrame(): Promise<{
			frame: number;
			group: bigint;
		} & Group.Frame | undefined>;
		readJson(): Promise<unknown | undefined>;
		readString(): Promise<string | undefined>;
		get subscription(): Signals.Getter<Track.Subscription | undefined>;
		tryNextGroup(): Group.Consumer | undefined;
		update(options: Track.Subscription): void;
		withRange(groups: Track.Groups): this;
	}
	export interface Position {
		frame: number;
		group: bigint;
	}
	export class Producer {
		constructor(name: string);
		abort(error: Error): void;
		accept(info?: Track.InfoInit): this;
		appendGroup(): Group.Producer;
		close(): void;
		get closed(): Signals.GetPromise<Error | null>;
		createGroup(sequence: bigint): Group.Producer;
		demand(): Track.Demand;
		finish(): Track.Finished;
		finishAt(end: Track.Position, error?: Error): void;
		info(): Promise<Track.Info>;
		largest(): Track.Position | undefined;
		readonly name: string;
		get priority(): number;
		requests(): Track.Requests;
		skipGroups(start: bigint, end: bigint): void;
		subscribe(options?: Track.Subscription): Track.Subscriber;
		get subscription(): Signals.Getter<Track.Subscription | undefined>;
		writeFrame(frame: Group.Frame): void;
	}
	export interface Range {
		end?: Track.Anchor;
		start?: Track.Anchor;
	}
	export class Request {
		accept(info?: Track.InfoInit): Track.Producer;
		demand(): Track.Demand;
		readonly name: string;
		get priority(): number;
		reject(err: Error): void;
		get subscription(): Readonly<Track.Subscription>;
	}
	export class Requests {
		[Symbol.dispose](): void;
		close(): void;
		get name(): string;
		requestedFill(): Promise<Track.Fill | undefined>;
	}
	export class Subscriber {
		close(abort?: Error): void;
		get closed(): Signals.GetPromise<Error | null>;
		finished(): Promise<Track.Position>;
		floor(): bigint | undefined;
		info(): Promise<Track.Info>;
		largest(): Promise<Track.Position | undefined>;
		readonly name: string;
		nextInRange(from: bigint): bigint | undefined;
		ordered(): Track.Ordered;
		recvGroup(): Promise<Group.Consumer | undefined>;
		start(): Promise<bigint | undefined>;
		get subscription(): Signals.Getter<Track.Subscription | undefined>;
		tryRecvGroup(): Group.Consumer | undefined;
		update(options: Track.Subscription): void;
		withRange(groups: Track.Groups): this;
	}
	export const Subscription: {
		default(): Track.Subscription;
	};
	export interface Subscription {
		delay?: Track.Delay;
		order?: Track.Order;
		priority?: number;
		ranges?: Track.Range[];
	}
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
