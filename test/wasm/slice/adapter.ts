/** What the slice bench needs from either generator, so one page drives both. */
export interface Slice {
	noop(): void;
	noopAsync(): Promise<void>;
	yieldOnce(): Promise<void>;
	/** A fresh standalone group, a consumer of it, and a way to free both. */
	group(): Group;
}

export interface Group {
	sequence(): bigint;
	write(bytes: Uint8Array): void;
	read(): Promise<number | undefined>;
	close(): void;
}
