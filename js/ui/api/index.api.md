# `@moq/ui`

```ts
import { Effect } from "@moq/signals";
import { Getter } from "@moq/signals";
import { Signal } from "@moq/signals";

export interface Fullscreen {
	active: Getter<boolean>;
	toggle(): void;
}

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

export interface StatCard {
	el: HTMLElement;
	grid: HTMLElement;
	status: HTMLElement;
}

export type StatKind = "network" | "video" | "audio";

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

export function createFullscreen(parent: Effect, player: HTMLElement): Fullscreen;

export function formatBitrate(bps: number): string;

export function formatFps(fps: number): string;

export function formatHz(hz: number): string;

export function formatMillis(ms: number): string;

export function fullscreenButton(parent: Effect, player: HTMLElement): HTMLButtonElement;

export function graph(parent: Effect, title: string, opts?: GraphOptions): Graph;

export function icon(svg: string): HTMLElement;

export function settingsButton(parent: Effect, panel: Signal<boolean>): HTMLButtonElement;

export function statCard(kind: StatKind, label: string, svg: string): StatCard;

export function statLine(grid: HTMLElement, label: string): HTMLSpanElement;
```
