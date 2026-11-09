# `@moq/boy/element`

```ts
import { Connection } from "@moq/net";
import { Origin } from "@moq/net";
import { Signal } from "@moq/signals";

export default class extends HTMLElement {
	constructor();
	attributeChangedCallback(name: "url" | "prefix" | "prefix-game", _oldValue: string | null, newValue: string | null): void;
	connectedCallback(): void;
	readonly connection: Connection;
	disconnectedCallback(): void;
	readonly expanded: Signal<string | undefined>;
	static observedAttributes: readonly ["url", "prefix", "prefix-game"];
	readonly origin: Origin.Producer;
	get prefixGame(): string;
	set prefixGame(value: string);
	get prefixPath(): string;
	set prefixPath(value: string);
	get url(): URL | undefined;
	set url(value: string | URL | undefined);
}
```
