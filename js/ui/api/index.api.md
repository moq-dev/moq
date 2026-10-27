# `@moq/ui`

```ts
import { Effect } from "@moq/signals";
import { Signal } from "@moq/signals";

export interface Graph {
	el: HTMLElement;
	push(value: number | undefined): void;
}

export interface GraphOptions {
	color?: string;
	format?: (v: number) => string;
	max?: number;
	samples?: number;
}

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

export function controlButton(svg: string, label: string): HTMLButtonElement;

export function graph(parent: Effect, title: string, opts?: GraphOptions): Graph;

export function icon(svg: string): HTMLElement;

export function settingsButton(parent: Effect, panel: Signal<boolean>): HTMLButtonElement;
```
