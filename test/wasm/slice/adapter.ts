/** What the slice harness needs from either generator, so one page drives both. */
export interface Slice {
	noop(): void;
	noopAsync(): Promise<void>;
	yieldOnce(): Promise<void>;
	/** A fresh standalone group at `sequence` (default: the next), and a consumer of it. */
	group(sequence?: bigint): Group;
	/** Dial `url` trusting only the hex SHA-256 `fingerprint`, offering only `version`. */
	connect(url: string, fingerprint: string, version: string): Promise<Session>;
}

/** A frame as both generators hand it back, payload copied out of Rust. */
export interface Frame {
	payload: Uint8Array;
	timestampUs?: bigint;
}

export interface Group {
	sequence(): bigint;
	write(bytes: Uint8Array, timestampUs?: bigint): void;
	read(signal?: AbortSignal): Promise<Frame | undefined>;
	/** Free the consumer's handle, leaving any pending call to the generator. */
	free(): void;
	close(): void;
}

export interface Session {
	/** Wait until `path` is announced. */
	broadcast(path: string): Promise<Broadcast>;
	/** Announce a broadcast at `path` with one microsecond-timed track, created first. */
	publish(path: string): Publisher;
	cancel(): void;
}

export interface Broadcast {
	subscribe(name: string): Promise<Track>;
}

export interface Track {
	/** The next group; rejects once the track ends. */
	recvGroup(signal?: AbortSignal): Promise<RemoteGroup>;
}

export interface RemoteGroup {
	sequence(): bigint;
	read(signal?: AbortSignal): Promise<Frame | undefined>;
}

export interface Publisher {
	group(sequence: bigint): { write(bytes: Uint8Array, timestampUs: bigint): void; finish(): void };
}

/** Each adapter bundle's one export. */
export type Open = (wasm: URL) => Promise<Slice>;
