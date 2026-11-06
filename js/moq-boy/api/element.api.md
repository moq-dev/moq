# `@moq/boy/element`

```ts
import { Game } from "@moq/boy";
import { Moq } from "@moq/boy";

export default class extends HTMLElement {
	constructor();
	attributeChangedCallback(name: "url" | "prefix" | "prefix-game", _oldValue: string | null, newValue: string | null): void;
	connectedCallback(): void;
	readonly connection: Moq.Connection;
	disconnectedCallback(): void;
	readonly expanded: Moq.Signals.Signal<string | undefined>;
	readonly games: Moq.Signals.Signal<ReadonlyMap<string, Game>>;
	static observedAttributes: readonly ["url", "prefix", "prefix-game"];
	readonly origin: Moq.Origin.Producer;
	get prefixGame(): string;
	set prefixGame(value: string);
	get prefixPath(): string;
	set prefixPath(value: string);
	get url(): URL | undefined;
	set url(value: string | URL | undefined);
}
```
