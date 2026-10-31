# `@moq/watch/element`

```ts
import { Audio } from "@moq/watch";
import { Broadcast } from "@moq/watch";
import { CatalogFormat } from "@moq/watch";
import { Clock } from "@moq/watch";
import { Delay } from "@moq/watch";
import { Hang } from "@moq/watch";
import { Net } from "@moq/watch";
import { Player } from "@moq/watch";
import { Signals } from "@moq/watch";
import { Sync } from "@moq/watch";
import { Text } from "@moq/watch";
import { Video } from "@moq/watch";

export default class extends HTMLElement {
	constructor();
	attributeChangedCallback(name: "delay" | "buffer" | "url" | "name" | "follow" | "volume" | "muted" | "paused" | "visible" | "backend" | "versions" | "announced" | "catalog-format" | "captions", oldValue: string | null, newValue: string | null): void;
	readonly audio: Audio.Decoder;
	get backend(): Video.Backend;
	set backend(value: Video.Backend);
	readonly broadcast: Broadcast;
	get buffer(): Clock.Milli;
	set buffer(value: Clock.Milli);
	get captions(): string | undefined;
	set captions(value: string | undefined);
	get catalog(): Hang.Catalog.Root | undefined;
	set catalog(value: Hang.Catalog.Root | undefined);
	get catalogFormat(): CatalogFormat | undefined;
	set catalogFormat(value: CatalogFormat | undefined);
	connectedCallback(): void;
	connection: Net.Connection;
	readonly controls: {
		audio: {
			muted: Signals.Signal<boolean>;
			target: Signals.Signal<Audio.Target | undefined>;
			volume: Signals.Signal<number>;
		};
		buffer: Signals.Signal<Clock.Milli>;
		delay: Signals.Signal<Delay>;
		paused: Signals.Signal<boolean>;
		text: {
			target: Signals.Signal<string | undefined>;
		};
		video: {
			backend: Signals.Signal<Video.Backend>;
			target: Signals.Signal<Video.Target | undefined>;
			visible: Signals.Signal<Video.Visible>;
		};
	};
	get delay(): Delay;
	set delay(value: Delay);
	disconnectedCallback(): void;
	emitter: Audio.Emitter;
	get follow(): boolean;
	set follow(value: boolean);
	get muted(): boolean;
	set muted(value: boolean);
	get name(): Net.Path.Valid;
	set name(value: string | Net.Path.Valid);
	static observedAttributes: readonly ["url", "versions", "name", "paused", "volume", "muted", "visible", "backend", "follow", "announced", "delay", "buffer", "catalog-format", "captions"];
	get paused(): boolean;
	set paused(value: boolean);
	readonly player: Player;
	readonly renderer: Video.Renderer;
	reset(): void;
	readonly signals: Signals.Effect;
	readonly sync: Sync;
	text: Text.Source;
	textRenderer: Text.Renderer;
	get url(): URL | undefined;
	set url(value: string | URL | undefined);
	get versions(): readonly string[] | undefined;
	set versions(value: readonly string[] | undefined);
	readonly video: Video.Decoder;
	get visible(): Video.Visible;
	set visible(value: Video.Visible);
	get volume(): number;
	set volume(value: number);
}

export function parseCatalogFormat(value: string | null): CatalogFormat | undefined;
```
