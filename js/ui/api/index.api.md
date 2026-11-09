# `@moq/ui`

```ts
export type Level = "full" | "partial" | "none";

export interface Report {
	rows: [group: string, feature: string, status: Status][];
	summary: Level;
}

export type Show = "always" | "warning" | "error" | "never";

export type Status = boolean | Level | {
	hardware?: boolean;
	software: boolean;
} | undefined;

export abstract class Support extends HTMLElement {
	constructor();
	attributeChangedCallback(name: "show" | "details", _oldValue: string | null, newValue: string | null): void;
	connectedCallback(): void;
	get details(): boolean;
	set details(details: boolean);
	protected abstract detect(): Promise<Report>;
	disconnectedCallback(): void;
	static observedAttributes: readonly ["show", "details"];
	get show(): Show;
	set show(show: Show);
}
```
