# `@moq/publish/element`

```ts
import { Audio } from "@moq/publish";
import { Broadcast } from "@moq/publish";
import { Net } from "@moq/publish";
import { Preview } from "@moq/publish";
import { Signals } from "@moq/publish";
import { Source } from "@moq/publish";
import { Video } from "@moq/publish";

export type AnnounceMode = "always" | "source";

export type IdleMode = "hold" | "release";

export type SourceType = "camera" | "screen" | "file";

export default class extends HTMLElement {
	constructor();
	get announce(): AnnounceMode;
	set announce(value: AnnounceMode);
	attributeChangedCallback(name: "source" | "url" | "stats" | "announce" | "name" | "backend" | "versions" | "muted" | "invisible" | "preview" | "echo-path" | "idle", oldValue: string | null, newValue: string | null): void;
	readonly audio: Audio.Encoder;
	get backend(): Preview.Backend;
	set backend(value: Preview.Backend);
	readonly broadcast: Broadcast;
	connectedCallback(): void;
	readonly connection: Net.Connection;
	readonly controls: {
		announce: Signals.Signal<AnnounceMode>;
		backend: Signals.Signal<Preview.Backend>;
		echoPath: Signals.Signal<Net.Path.Relative | undefined>;
		idle: Signals.Signal<IdleMode>;
		invisible: Signals.Signal<boolean>;
		muted: Signals.Signal<boolean>;
		preview: Signals.Signal<Preview.Mode>;
		source: Signals.Signal<File | SourceType | undefined>;
		stats: Signals.Signal<boolean>;
	};
	disconnectedCallback(): void;
	get echoPath(): Net.Path.Relative | undefined;
	set echoPath(value: string | undefined);
	get idle(): IdleMode;
	set idle(value: IdleMode);
	get invisible(): boolean;
	set invisible(value: boolean);
	get muted(): boolean;
	set muted(value: boolean);
	get name(): Net.Path.Valid;
	set name(value: string | Net.Path.Valid);
	static observedAttributes: readonly ["url", "versions", "name", "muted", "invisible", "source", "preview", "backend", "announce", "stats", "echo-path", "idle"];
	get preview(): Preview.Mode;
	set preview(value: Preview.Mode);
	readonly signals: Signals.Effect;
	get source(): SourceType | File | undefined;
	set source(value: SourceType | File | undefined);
	readonly sources: Signals.Readonlys<{
		audio: Signals.Signal<Source.Microphone | Source.Screen | undefined>;
		file: Signals.Signal<Source.File | undefined>;
		video: Signals.Signal<Source.Camera | Source.Screen | undefined>;
	}>;
	get stats(): boolean;
	set stats(value: boolean);
	get url(): URL | undefined;
	set url(value: string | URL | undefined);
	get versions(): readonly string[] | undefined;
	set versions(value: readonly string[] | undefined);
	readonly video: Video.Encoder;
}
```
