# `@moq/obs`

```ts
import { GetPromise } from "@moq/signals";
import { Getter } from "@moq/signals";

export interface Configured {
	encoder: string;
	ignored: Ignored[];
	profile: string;
}

export class ConnectError extends Error {
	constructor(reason: ConnectReason, message: string);
	readonly reason: ConnectReason;
}

export interface ConnectProps {
	password?: string;
	port?: number;
}

export type ConnectReason = "denied" | "unreachable" | "refused" | "blocked" | "unsupported";

export type Destination = {
	broadcast: string;
	kind: "moq";
	layers?: number;
	token?: string;
	url: string | URL;
} | {
	key: string;
	kind: "rtmp";
	server: string | URL;
};

export type Ignored = "bframes" | "keyint" | "lookahead";

export class Obs {
	addSource(source: Source): Promise<void>;
	close(): void;
	readonly closed: GetPromise<Error | null>;
	configure(publish: Publish): Promise<Configured>;
	readonly plugin: boolean;
	start(): Promise<void>;
	readonly stats: Getter<Stats | undefined>;
	stop(): Promise<void>;
	readonly stream: Getter<StreamState>;
	readonly version: Version;
}

export const PORT: 4455;

export interface Publish {
	audio?: {
		bitrate?: number;
	};
	destination: Destination;
	profile?: string;
	video?: {
		bitrate?: number;
		fps?: number;
		size?: {
			height: number;
			width: number;
		};
	};
}

export class RequestError extends Error {
	constructor(request: string, code: number, comment: string | undefined);
	readonly code: number;
	readonly request: string;
}

export interface Source {
	broadcast: string;
	name?: string;
	scene?: string;
	url: string | URL;
}

export interface Stats {
	bitrate: number | undefined;
	bytes: number;
	congestion: number;
	frames: {
		dropped: number;
		skipped: number;
		total: number;
	};
}

export type StreamState = "stopped" | "starting" | "live" | "reconnecting" | "stopping";

export interface Version {
	obs: string;
	websocket: string;
}

export function connect(props?: ConnectProps): Promise<Obs>;
```
