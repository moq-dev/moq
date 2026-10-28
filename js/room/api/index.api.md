# `@moq/room`

```ts
import { Audio } from "@moq/watch";
import { Connection } from "@moq/net";
import { Effect } from "@moq/signals";
import { Getter } from "@moq/signals";
import { GetterInit } from "@moq/signals";
import { Path } from "@moq/net";
import { Player } from "@moq/watch";
import { Readonlys } from "@moq/signals";
import { Root } from "@moq/hang/catalog";
import { Signal } from "@moq/signals";
import { Source } from "@moq/publish";
import { Timed } from "@moq/net";
import { Track } from "@moq/net";
import { Window } from "@moq/json";

export namespace Chat {
	export type Event = Window.Event<string>;
	export const HISTORY: Clock.Nano;
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

export class Local {
	constructor(props: LocalProps);
	readonly camera: import("@moq/publish").Broadcast;
	readonly cameraCapture: import("@moq/publish").Video.Capture;
	readonly cameraEnabled: Signal<boolean>;
	readonly chatting: Signal<boolean>;
	close(): void;
	readonly enabled: Signal<boolean>;
	readonly identity: Getter<Path.Valid>;
	readonly microphone: Source.Microphone;
	readonly microphoneEnabled: Signal<boolean>;
	get preview(): Getter<Preview>;
	readonly screen: import("@moq/publish").Broadcast;
	readonly screenCapture: import("@moq/publish").Video.Capture;
	readonly screenEnabled: Signal<boolean>;
	readonly share: Source.Screen;
	readonly typing: Signal<boolean>;
	readonly user: UserFields;
	readonly webcam: Source.Camera;
}

export interface LocalProps {
	cameraEnabled?: GetterInit<boolean>;
	connection: Connection;
	enabled?: GetterInit<boolean>;
	identity: GetterInit<Path.Valid>;
	microphoneEnabled?: GetterInit<boolean>;
	screenEnabled?: GetterInit<boolean>;
	user?: UserProps;
}

export class Member {
	constructor(kind: Kind, path: Path.Valid, connection: Connection);
	readonly audio: Audio.Decoder;
	readonly broadcast: import("@moq/watch").Broadcast;
	readonly canvas: Signal<HTMLCanvasElement | undefined>;
	close(): void;
	readonly emitter: Audio.Emitter;
	readonly kind: Kind;
	readonly muted: Signal<boolean>;
	readonly path: Path.Valid;
	readonly player: Player;
	readonly preview: Getter<Preview>;
	readonly renderer: import("@moq/watch").Video.Renderer;
	readonly user: Readonlys<UserInput>;
	readonly video: import("@moq/watch").Video.Decoder;
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
	get remotes(): Getter<Map<Path.Valid, Remote>>;
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
