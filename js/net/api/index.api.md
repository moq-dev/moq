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
		kind: "present" | "absent";
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
		readonly closed: Signals.GetPromise<Error | null>;
		readonly grant: Signals.Getter<Auth.Grant | undefined>;
	}
	export class Duplicate extends Error {
		constructor(message?: string);
	}
	export interface Grant {
		expires?: Clock.Wall;
		publish: Path.Prefixes;
		subscribe: Path.Prefixes;
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
		get identity(): Identity.Valid | undefined;
		get path(): Path.Valid;
		track(name: string): Track.Consumer;
		tracks(): Broadcast.Tracks;
	}
	export type Listed = {
		datagrams?: false;
		info: Track.Info;
	} | {
		datagrams: true;
		info: Datagram.Info;
	};
	export class Producer {
		constructor();
		close(): void;
		get closed(): Signals.GetPromise<null>;
		consume(): Broadcast.Consumer;
		createDatagrams(name: string, info?: Partial<Datagram.Info>): Datagram.Producer | undefined;
		createTrack(name: string, info?: Track.InfoInit): Track.Producer | undefined;
		removeTrack(name: string): void;
		track(name: string): Track.Consumer;
	}
	export class Tracks {
		[Symbol.asyncIterator](): AsyncGenerator<Broadcast.TracksEvent, void, undefined>;
		close(): void;
		next(): Promise<Broadcast.TracksEvent | undefined>;
	}
	export type TracksEvent = {
		kind: "added";
		name: string;
	} & Broadcast.Listed | {
		kind: "synced";
	} | {
		datagrams?: boolean;
		kind: "removed";
		name: string;
	} | {
		datagrams?: boolean;
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
		urlAuth?: boolean;
		versions?: readonly string[];
		viewers?: Bandwidth.Viewers;
		websocket?: Connection.WebSocketProps;
		webtransport?: Connection.WebTransportProps;
	}
	export class Dial {
		fail(err: Error): void;
		readonly failure?: Error;
		present(tokens: readonly string[]): void;
		readonly signal: AbortSignal;
		readonly url: URL;
	}
	export let Dials: typeof Connection.Dials;
	export class Dials {
		constructor();
		next(): Promise<Connection.Dial | undefined>;
	}
	export interface Goaway {
		readonly timeout?: Clock.Duration;
		readonly uri: string;
	}
	export interface Handover {
		handover?: Clock.Duration;
		redirect?: Connection.Redirect;
	}
	export interface Probe {
		estimatedRecvRate?: number;
		rtt?: Clock.Duration;
	}
	export interface Props {
		backoff?: Connection.Backoff;
		consume?: Origin.Producer;
		enabled?: boolean | Signals.Signal<boolean>;
		goaway?: Connection.Handover;
		publish?: Origin.Consumer;
		share?: boolean;
		signal?: never;
		timeout?: Clock.Duration;
		tokens?: readonly string[] | Signals.Signal<readonly string[]> | Connection.Dials;
		transport?: never;
		url?: URL | Signals.Signal<URL | undefined>;
		urlAuth?: boolean;
		versions?: readonly string[] | Signals.Signal<readonly string[] | undefined>;
		websocket?: Connection.WebSocketProps;
		webtransport?: Connection.WebTransportProps;
	}
	export type Redirect = "follow" | "same-host" | "ignore";
	export interface Session {
		abort(): void;
		announced(filter?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
		readonly auth: Auth.Handle;
		close(): Promise<void>;
		readonly closed: Signals.GetPromise<Error | null>;
		readonly draining: Signals.GetPromise<Connection.Goaway>;
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
	export type Transport = "webtransport" | "websocket";
	export interface WebSocketProps {
		delay?: Clock.Duration;
		enabled?: boolean;
		idleTimeout?: Clock.Duration | null;
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
	announced(filter?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
	answered(): Promise<void>;
	readonly auth: Signals.Getter<Auth.Handle | undefined>;
	static readonly certificateHash: (cert: string | BufferSource) => Promise<Uint8Array<ArrayBuffer>>;
	static readonly checkVersions: (versions: readonly string[]) => string[];
	close(abort?: Error): void;
	get closed(): Signals.GetPromise<Error | null>;
	static readonly connect: (props: Connection.ConnectProps) => Promise<Connection.Session>;
	enabled: Signals.Signal<boolean>;
	readonly error: Signals.Getter<Error | undefined>;
	readonly failure: Signals.Getter<Error | undefined>;
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
		timestamp: Time.Timestamp;
	}
	export interface Info {
		priority: number;
		timescale: Time.Timescale | null;
	}
	export class Producer {
		constructor(name: string);
		abort(error: Error): void;
		accept(properties?: Partial<Datagram.Info>): this;
		append(timestamp: Time.Timestamp, payload: Uint8Array): bigint;
		get closed(): Signals.GetPromise<Error | null>;
		consume(): Datagram.Consumer;
		finish(): void;
		info(): Promise<Datagram.Info>;
		insert(datagram: Datagram.Datagram): void;
		readonly name: string;
		get priority(): Signals.Getter<number | undefined>;
	}
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
	export const Retry: Readonly<{
		readonly Never: "never";
		readonly Reroute: "reroute";
		readonly Same: "same";
	}>;
	export type Retry = typeof Error.Retry[keyof typeof Error.Retry];
	export class Session extends Error {
		constructor(code: SessionCode, options?: {
			cause?: unknown;
			reason?: string;
		});
		readonly code: SessionCode;
	}
	export class Skipped extends Error {
		constructor(start: bigint);
		readonly start: bigint;
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
	export function retry(err: unknown): Error.Retry;
}

export namespace Frame {
	export class Consumer {
		get arrived(): Clock.Instant | undefined;
		readonly index: number;
		readAll(): Promise<Uint8Array>;
		readChunk(): Promise<Uint8Array | undefined>;
		readonly size: number;
		readonly timestamp: Time.Timestamp;
	}
	export interface Info {
		size: number;
		timestamp: Time.Timestamp;
	}
	export class Producer {
		abort(error: Error): void;
		finish(): void;
		get remaining(): number;
		readonly size: number;
		readonly timestamp: Time.Timestamp;
		write(chunk: Uint8Array): void;
	}
}

export namespace Group {
	export class Consumer {
		get arrived(): Clock.Instant;
		close(): void;
		get closed(): Signals.GetPromise<Error | null>;
		get done(): boolean;
		get end(): bigint | undefined;
		get frameCount(): number;
		nextFrame(): Promise<Frame.Consumer | undefined>;
		readFrame(): Promise<Group.Frame | undefined>;
		readFrameIndex(options?: Group.ReadOptions): Promise<{
			index: number;
		} & Group.Frame | undefined>;
		readJson(): Promise<unknown | undefined>;
		readString(): Promise<string | undefined>;
		readable(): Promise<void>;
		readonly start: bigint;
		tryReadFrame(): Group.Frame | undefined;
		tryReadFrameIndex(): {
			index: number;
		} & Group.Frame | undefined;
	}
	export interface Finished {
		readonly closed: Signals.GetPromise<Error | null>;
		consume(): Group.Consumer;
		readonly start: bigint;
	}
	export interface Frame {
		arrived?: Clock.Instant;
		payload: Uint8Array;
		timestamp: Time.Timestamp;
	}
	export interface Info {
		start: bigint;
	}
	export class Producer {
		constructor(start: bigint);
		abort(error: Error): void;
		get closed(): Signals.GetPromise<Error | null>;
		consume(): Group.Consumer;
		createFrame(info: Frame.Info): Frame.Producer;
		finish(): Group.Finished;
		finishAt(end: bigint): Group.Finished;
		readonly start: bigint;
		writeFrame(frame: Group.Frame): void;
	}
	export interface ReadOptions {
		from?: number;
	}
}

export namespace Identity {
	export interface Minted {
		get text(): string;
		toString(): string;
	}
	export type Named = string & {
		readonly _brand: "identity";
	};
	export type Valid = Identity.Named | Identity.Minted;
	export function isMinted(identity: Identity.Valid): identity is Identity.Minted;
	export function parse(text: string): Identity.Named;
	export function random(): Identity.Named;
	export function text(identity: Identity.Valid): string;
}

export namespace Origin {
	export class Claim {
		[Symbol.dispose](): void;
		announce(): void;
		get announced(): boolean;
		close(): void;
		get identity(): Identity.Valid;
		requested(): AsyncIterableIterator<Origin.Request>;
		setAuth(auth?: Auth.Credential): void;
		setCost(cost: Origin.Cost): void;
		withdraw(): void;
	}
	export class Consumer implements Origin.Reader {
		announced(filter?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
		get closed(): Signals.GetPromise<Error | null>;
		get discovery(): Signals.Getter<boolean | undefined>;
		follow(path: Path.Valid, options?: Origin.FollowOptions): Origin.Following;
		request(path: Path.Valid, options?: Origin.RequestOptions): Origin.Requesting;
	}
	export type Cost = bigint;
	export interface FollowOptions {
		identity?: Identity.Valid;
	}
	export class Following {
		readonly active: Signals.Getter<Broadcast.Consumer | undefined>;
		close(): void;
		readonly closed: Signals.GetPromise<Error | null>;
		readonly path: Path.Valid;
	}
	export class Producer implements Origin.Table {
		constructor();
		announced(filter?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
		claim(prefix: Path.Valid, options?: Origin.RouteOptions): Origin.Claim;
		close(abort?: Error): void;
		get closed(): Signals.GetPromise<Error | null>;
		consume(): Origin.Consumer;
		get discovery(): Signals.Getter<boolean | undefined>;
		follow(path: Path.Valid, options?: Origin.FollowOptions): Origin.Following;
		request(path: Path.Valid, options?: Origin.RequestOptions): Origin.Requesting;
		route(path: Path.Valid, options?: Origin.RouteOptions): Origin.Route;
		scope(root: Path.Valid, prefixes: Path.Prefixes): Origin.Producer;
	}
	export interface Reader {
		announced(filter?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
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
		unwanted(): Promise<void>;
	}
	export interface RequestOptions {
		identity?: Identity.Valid;
	}
	export class Requesting {
		readonly active: Signals.Getter<Broadcast.Consumer | undefined>;
		broadcast(): Promise<Broadcast.Consumer>;
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
		get identity(): Identity.Valid;
		setAuth(auth?: Auth.Credential): void;
		setCost(cost: Origin.Cost): void;
		withdraw(): void;
	}
	export interface RouteInfo {
		readonly available: boolean;
		readonly cost: Origin.Cost;
		readonly identity?: Identity.Valid;
	}
	export interface RouteOptions {
		auth?: Auth.Credential;
		cost?: Origin.Cost;
		hidden?: boolean;
		identity?: {
			epoch: bigint;
			id: Identity.Valid;
		};
	}
	export interface Table extends Origin.Reader {
		claim(prefix: Path.Valid, options?: Origin.RouteOptions): Origin.Claim;
		route(path: Path.Valid, options?: Origin.RouteOptions): Origin.Route;
	}
}

export namespace Path {
	export { InvalidPattern } from "@moq/pattern";
	export { InvalidPrefix } from "@moq/pattern";
	export { Pattern } from "@moq/pattern";
	export { Patterns } from "@moq/pattern";
	export { Prefix } from "@moq/pattern";
	export { Prefixes } from "@moq/pattern";
	export type Relative = string & {
		__brand: "Relative";
	};
	export { Segment } from "@moq/pattern";
	export { Specificity } from "@moq/pattern";
	export type Valid = string & {
		__brand: "Name";
	};
	export { compareSpecificity } from "@moq/pattern";
	export function empty(): Path.Valid;
	export { escape } from "@moq/pattern";
	export function from(...paths: string[]): Path.Valid;
	export function hasPrefix(prefix: Path.Valid, path: Path.Valid): boolean;
	export function head(path: Path.Valid, n: number): Path.Valid | undefined;
	export function join(path: Path.Valid, other: Path.Valid): Path.Valid;
	export function normalizeRelative(rel: string): Path.Relative;
	export function parts(path: Path.Valid): string[];
	export function relative(target: Path.Valid, base: Path.Valid): Path.Relative | undefined;
	export function resolve(base: Path.Valid, rel: Path.Relative): Path.Valid;
	export function stripPrefix(prefix: Path.Valid, path: Path.Valid): Path.Valid | null;
	export function tryResolve(base: Path.Valid, rel: Path.Relative): Path.Valid | undefined;
	export { unescape } from "@moq/pattern";
}

export const SessionCode: ((code: number) => SessionCode) & {
	readonly Cancel: SessionCode;
	readonly Expired: SessionCode;
	readonly GoawayTimeout: SessionCode;
	readonly Internal: SessionCode;
	readonly MalformedAuthToken: SessionCode;
	readonly ProtocolViolation: SessionCode;
	readonly Timeout: SessionCode;
	readonly TooManyRequests: SessionCode;
	readonly Unauthorized: SessionCode;
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
	readonly Skip: StreamCode;
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
		get key(): bigint | undefined;
		static now(): Time.Timestamp;
		readonly scale: Time.Timescale | undefined;
		static unscaled(key: bigint | number): Time.Timestamp;
		readonly value: number;
	}
}

export interface Timed<T> {
	at: Time.Timestamp;
	value: T;
}

export namespace Track {
	export type Bound = {
		excluded?: never;
		included: bigint;
	} | {
		excluded: bigint;
		included?: never;
	};
	export type Compression = "none" | "deflate";
	export class Consumer {
		fetchGroup(key: bigint, options?: Track.FetchGroupOptions): Promise<Group.Consumer>;
		info(options?: Track.InfoOptions): Promise<Track.Info>;
		readonly name: string;
		subscribe(options?: Track.Subscription): Track.Subscriber;
	}
	export class Demand {
		get closed(): Signals.GetPromise<Error | null>;
		readonly name: string;
		get refresh(): Signals.Getter<boolean>;
		get wanted(): Signals.Getter<boolean>;
	}
	export interface FetchGroupOptions {
		priority?: number;
		signal?: AbortSignal;
	}
	export class FillGroups {
		[Symbol.dispose](): void;
		accept(start: bigint, info?: Track.Info): Group.Producer | undefined;
		close(): void;
		dropGroups(start: bigint, end: bigint): void;
		readonly end: bigint;
		readonly floor: bigint;
		isWanted(key: bigint): boolean;
		get order(): Track.Order;
		get priority(): number;
		refuseGroups(start: bigint, end: bigint, error: Error): void;
		readonly start: bigint;
		unwanted(key: bigint): Promise<void>;
		wanted(): bigint | undefined;
		wantedChanged(): Promise<bigint | undefined>;
	}
	export interface Finished {
		abort(error: Error): void;
		close(): void;
		readonly closed: Signals.GetPromise<Error | null>;
		createGroup(key: bigint): Group.Producer;
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
		refresh: Track.Refresh;
		timescale: Time.Timescale | null;
	}
	export interface InfoInit extends Partial<Omit<Track.Info, "refresh">> {
		maxAge?: Clock.Duration | null;
		refresh?: Partial<Track.Refresh>;
	}
	export interface InfoOptions {
		signal?: AbortSignal;
	}
	export type Item = {
		group: Group.Consumer;
		kind: "group";
	} | {
		end: bigint;
		error: Error;
		group?: never;
		kind: "gap";
		landed?: Clock.Instant;
		start: bigint;
	};
	export interface Live {
		max?: Clock.Duration | null;
		min: Clock.Duration | null;
	}
	export type Order = "ascending" | "descending";
	export class Ordered {
		close(abort?: Error): void;
		get closed(): Signals.GetPromise<Error | null>;
		finished(): Promise<Track.Position>;
		info(): Promise<Track.Info>;
		isSkipped(start: bigint, end: bigint): boolean;
		largest(): Promise<Track.Position | undefined>;
		readonly name: string;
		nextGroup(): Promise<Group.Consumer | undefined>;
		nextItem(): Promise<Track.Item | undefined>;
		readFrame(): Promise<{
			frame: number;
			group: bigint;
		} & Group.Frame | undefined>;
		readJson(): Promise<unknown | undefined>;
		readString(): Promise<string | undefined>;
		refresh(options?: Track.RefreshOptions): Promise<void>;
		get subscription(): Signals.Getter<Track.Subscription | undefined>;
		tryNextGroup(): Group.Consumer | undefined;
		tryNextItem(): Track.Item | undefined;
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
		appendGroup(start: Time.Timestamp): Group.Producer;
		close(): void;
		get closed(): Signals.GetPromise<Error | null>;
		createGroup(key: bigint): Group.Producer;
		demand(): Track.Demand;
		fail(error: Error): Track.Finished;
		finish(): Track.Finished;
		finishAt(end: Track.Position): void;
		info(): Promise<Track.Info>;
		largest(): Track.Position | undefined;
		readonly name: string;
		get priority(): number;
		requests(): Track.Requests;
		skipGroups(start: bigint, end: bigint): void;
		subscribe(options?: Track.Subscription): Track.Subscriber;
		get subscription(): Signals.Getter<Track.Subscription | undefined>;
		get timescale(): Time.Timescale | null | undefined;
		writeFrame(frame: Group.Frame): void;
	}
	export interface Range {
		end?: bigint;
		start?: Track.Start;
	}
	export interface Refresh {
		interval: Clock.Duration | null;
		ratio: number | null;
	}
	export interface RefreshOptions {
		signal?: AbortSignal;
	}
	export class Requests {
		[Symbol.dispose](): void;
		close(): void;
		get name(): string;
		requestedFill(): Promise<Track.FillGroups | undefined>;
	}
	export type Start = bigint | {
		live: Track.Live;
	};
	export class Subscriber {
		close(abort?: Error): void;
		get closed(): Signals.GetPromise<Error | null>;
		finished(): Promise<Track.Position>;
		floor(): bigint | undefined;
		info(): Promise<Track.Info>;
		largest(): Promise<Track.Position | undefined>;
		readonly name: string;
		ordered(): Track.Ordered;
		recvGroup(): Promise<Group.Consumer | undefined>;
		refresh(options?: Track.RefreshOptions): Promise<void>;
		get subscription(): Signals.Getter<Track.Subscription | undefined>;
		update(options: Track.Subscription): void;
		withRange(groups: Track.Groups): this;
	}
	export const Subscription: {
		default(): Track.Subscription;
	};
	export interface Subscription {
		order?: Track.Order;
		priority?: number;
		ranges?: Track.Range[];
	}
	export function rescale(info: {
		timescale?: Time.Timescale | null;
	}, timestamp: Time.Timestamp): Time.Timestamp;
	export function rescaleIn(info: {
		timescale?: Time.Timescale | null;
	}, timestamp: Time.Timestamp, key: bigint): Time.Timestamp;
}

export namespace Varint {
	export function decode(buf: Uint8Array): [number, Uint8Array];
	export function decodeBigInt(buf: Uint8Array): [bigint, Uint8Array];
	export function encode(v: number | bigint): Uint8Array;
}
```
