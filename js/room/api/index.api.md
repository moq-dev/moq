# `@moq/room`

```ts
import { Audio } from "@moq/watch";
import { Connection } from "@moq/net";
import { Effect } from "@moq/signals";
import { Getter } from "@moq/signals";
import { GetterInit } from "@moq/signals";
import { Path } from "@moq/net";
import { Readonlys } from "@moq/signals";
import { Root } from "@moq/hang/catalog";
import { Signal } from "@moq/signals";
import { Source } from "@moq/publish";
import { Timed } from "@moq/net";
import { Track } from "@moq/net";
import { Window } from "@moq/json";

export namespace Chat {
	export type Event = Window.Event<string>;
	export const HISTORY: Clock.Duration;
	export const PRIORITY: 10;
	export class Publisher {
		constructor(track: Track.Producer);
		static create(broadcast: import("@moq/net").Broadcast.Producer): Chat.Publisher;
		expire(): void;
		finish(): void;
		send(text: string): void;
	}
	export class Subscriber {
		constructor(track: Track.Subscriber);
		close(): void;
		recv(): Promise<Timed<Chat.Event> | undefined>;
		static subscribe(broadcast: import("@moq/net").Broadcast.Consumer): Chat.Subscriber;
	}
	export const TRACK: "chat";
	export function info(): Pick<Track.Info, "priority" | "timescale">;
}

export type Claims = {
	publish: string[];
	root: string;
	subscribe: string[];
};

export * as Clock from "@moq/clock";

export type Consumed = {
	close: () => void;
	preview: Getter<Preview>;
	user: Readonlys<UserInput>;
};

export type ExtendedCatalog = Root & {
	hang?: HangCatalog;
};

export type HangCatalog = {
	chat?: TrackRef;
	location?: TrackRef;
	preview?: TrackRef;
	user?: TrackRef;
};

export const KIND: {
	readonly camera: "camera";
	readonly screen: "screen";
};

export type Kind = typeof KIND[keyof typeof KIND];

export namespace Local {
	export interface Camera {
		readonly broadcast: import("@moq/publish").Broadcast;
		readonly capture: import("@moq/publish").Video.Capture;
		readonly enabled: Signal<boolean>;
		readonly source: Source.Camera;
	}
	export interface Microphone {
		readonly enabled: Signal<boolean>;
		readonly join: Signal<Clock.Duration>;
		readonly root: Getter<AudioNode | undefined>;
		readonly source: Source.Microphone;
		readonly volume: Signal<number>;
	}
	export interface Screen {
		readonly audio: {
			readonly join: Signal<Clock.Duration>;
			readonly root: Getter<AudioNode | undefined>;
		};
		readonly broadcast: import("@moq/publish").Broadcast;
		readonly capture: import("@moq/publish").Video.Capture;
		readonly enabled: Signal<boolean>;
		readonly source: Source.Screen;
	}
}

export class Local {
	constructor(props: LocalProps);
	readonly camera: Local.Camera;
	readonly chatting: Signal<boolean>;
	close(): void;
	readonly enabled: Signal<boolean>;
	readonly identity: Getter<Path.Valid>;
	readonly microphone: Local.Microphone;
	get preview(): Getter<Preview>;
	readonly screen: Local.Screen;
	readonly typing: Signal<boolean>;
	readonly user: UserFields;
}

export interface LocalProps {
	camera?: {
		enabled?: GetterInit<boolean>;
	};
	connection: Connection;
	enabled?: GetterInit<boolean>;
	identity: GetterInit<Path.Valid>;
	microphone?: {
		enabled?: GetterInit<boolean>;
		join?: Clock.Duration | Signal<Clock.Duration>;
	};
	screen?: {
		audio?: {
			join?: Clock.Duration | Signal<Clock.Duration>;
		};
		enabled?: GetterInit<boolean>;
	};
	user?: UserProps;
}

export class Member {
	constructor(kind: Kind, path: Path.Valid, connection: Connection);
	readonly audio: Readonlys<{
		stats: Getter<Audio.Stats | undefined>;
		timestamp: Getter<Clock.Duration | undefined>;
	}>;
	readonly canvas: Signal<HTMLCanvasElement | undefined>;
	close(): void;
	readonly kind: Kind;
	readonly muted: Signal<boolean>;
	readonly path: Path.Valid;
	readonly preview: Getter<Preview>;
	readonly user: Readonlys<UserInput>;
	readonly video: Readonlys<{
		stats: Getter<import("@moq/watch").Video.Stats | undefined>;
		timestamp: Getter<Clock.Duration | undefined>;
	}>;
	readonly volume: Signal<number>;
}

export const PRIORITY: 90;

export type Parsed = {
	identity: Path.Valid;
	kind: Kind;
};

export type Preview = {
	audio?: boolean;
	avatar?: string;
	chat?: boolean;
	name?: string;
	screen?: boolean;
	typing?: boolean;
	video?: boolean;
};

export class Remote {
	constructor(props: RemoteProps);
	attach(kind: Kind, path: Path.Valid): void;
	readonly camera: Getter<Member | undefined>;
	close(): void;
	detach(kind: Kind): void;
	empty(): boolean;
	readonly identity: Path.Valid;
	readonly preview: Getter<Preview>;
	readonly screen: Getter<Member | undefined>;
	readonly user: Readonlys<UserInput>;
}

export interface RemoteProps {
	connection: Connection;
	identity: Path.Valid;
}

export class Room {
	constructor(props: RoomProps);
	close(): void;
	readonly connection: Connection;
	readonly enabled: Getter<boolean>;
	readonly identity: Getter<Path.Valid | undefined>;
	readonly prefix: Getter<Path.Valid | undefined>;
	get remotes(): Getter<ReadonlyMap<Path.Valid, Remote>>;
}

export interface RoomProps {
	connection: Connection;
	enabled?: GetterInit<boolean>;
	identity?: GetterInit<Path.Valid | undefined>;
	prefix?: GetterInit<Path.Valid | undefined>;
}

export const TRACK: {
	readonly chat: "hang/chat.json";
	readonly location: "hang/location.json";
	readonly preview: "hang/preview.json";
	readonly user: "hang/user.json";
};

export type TrackRef = {
	track: string;
};

export type User = {
	avatar?: string;
	color?: string;
	id?: string;
	name?: string;
};

export type UserFields = {
	avatar: Signal<string | undefined>;
	color: Signal<string | undefined>;
	id: Signal<string | undefined>;
	name: Signal<string | undefined>;
};

export type UserInput = {
	avatar: Getter<string | undefined>;
	color: Getter<string | undefined>;
	id: Getter<string | undefined>;
	name: Getter<string | undefined>;
};

export type UserProps = {
	avatar?: string | Signal<string | undefined>;
	color?: string | Signal<string | undefined>;
	id?: string | Signal<string | undefined>;
	name?: string | Signal<string | undefined>;
};

export function broadcastPath(identity: Path.Valid, kind: Kind): Path.Valid;

export function claims(room: string, identity: string): Claims;

export function consume(broadcast: import("@moq/watch").Broadcast): Consumed;

export function isKind(value: string): value is Kind;

export function kindFromSegment(segment: string): Kind | undefined;

export function parse(path: Path.Valid): Parsed | undefined;

export function serve(broadcast: import("@moq/publish").Broadcast, user: UserInput, preview: Getter<Preview>, effect: Effect): void;

export function userFields(props?: UserProps): UserFields;
```
