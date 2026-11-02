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

export type SourceType = "camera" | "screen" | "file";

export default class extends HTMLElement {
	constructor();
	get announce(): AnnounceMode | undefined;
	set announce(value: AnnounceMode);
	attributeChangedCallback(name: "source" | "url" | "announce" | "name" | "backend" | "versions" | "muted" | "invisible" | "preview", oldValue: string | null, newValue: string | null): void;
	audio: Audio.Encoder;
	get backend(): Preview.Backend;
	set backend(value: Preview.Backend);
	broadcast: Broadcast;
	connectedCallback(): void;
	connection: Net.Connection;
	readonly controls: {
		announce: Signals.Signal<AnnounceMode | undefined>;
		backend: Signals.Signal<Preview.Backend>;
		invisible: Signals.Signal<boolean>;
		muted: Signals.Signal<boolean>;
		preview: Signals.Signal<Preview.Mode>;
		source: Signals.Signal<File | SourceType | undefined>;
	};
	disconnectedCallback(): void;
	get invisible(): boolean;
	set invisible(value: boolean);
	get muted(): boolean;
	set muted(value: boolean);
	get name(): Net.Path.Valid;
	set name(value: string | Net.Path.Valid);
	static observedAttributes: readonly ["url", "versions", "name", "muted", "invisible", "source", "preview", "backend", "announce"];
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
	get url(): URL | undefined;
	set url(value: string | URL | undefined);
	get versions(): readonly string[] | undefined;
	set versions(value: readonly string[] | undefined);
	video: Video.Encoder;
}
```
