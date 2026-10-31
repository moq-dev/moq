/**
 * The part of the ubrn-generated moq-ffi bindings the baseline consumer reads through.
 *
 * Declared here rather than imported, because the generated module only exists after
 * `uniffi/build.sh`. `uniffi/check.ts` proves the generated module satisfies it.
 */
export interface Uniffi<Config = unknown, Tls = unknown> {
	uniffiInitAsync(source: URL): Promise<void>;
	MoqClient: new (config: Config) => { connect(url: string): Promise<Session> };
	MoqClientConfig: { create(fields: { versions: string[]; tls: Tls }): Config };
	MoqClientTls: { create(fields: { fingerprints: string[] }): Tls };
}

interface Session {
	consume(): { follow(path: string): { available(): Promise<Broadcast> } };
	cancel(code: number): void;
}

interface Broadcast {
	subscribeTrack(name: string, subscription: undefined): Promise<Track>;
}

interface Track {
	recvGroup(): Promise<Group | undefined>;
	cancel(): void;
}

interface Group {
	sequence(): bigint;
	readFrame(): Promise<{ payload: ArrayBuffer; timestampUs?: bigint } | undefined>;
	cancel(): void;
}
