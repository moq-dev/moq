# `@moq/signals`

```ts
import { Nano } from "@moq/clock";

export class Computed<T> implements Getter<T | undefined> {
	constructor(fn: (effect: Effect) => T);
	readonly [GETTER_BRAND]: true;
	changed(): Promise<T | undefined>;
	changed(fn: Subscriber<T | undefined>): Dispose;
	close(): void;
	peek(): T | undefined;
	subscribe(fn: Subscriber<T | undefined>): Dispose;
}

export class Derived<const S extends readonly Getter<unknown>[], T> implements Getter<T> {
	constructor(sources: S, fn: (...values: GetterValues<S>) => T);
	readonly [GETTER_BRAND]: true;
	changed(): Promise<T>;
	changed(fn: Subscriber<T>): Dispose;
	peek(): T;
	subscribe(fn: Subscriber<T>): Dispose;
}

export type Dispose = () => void;

export class Effect {
	constructor(fn?: (effect: Effect) => void);
	get abort(): AbortSignal;
	animate(fn: (now: number) => void): void;
	cleanup(fn: Dispose): void;
	close(): void;
	get closed(): Promise<void>;
	computed<T>(fn: (effect: Effect) => T): Computed<T>;
	event<K extends keyof HTMLElementEventMap>(target: HTMLElement, type: K, listener: (this: HTMLElement, ev: HTMLElementEventMap[K]) => void, options?: boolean | AddEventListenerOptions): void;
	event<K extends keyof SVGElementEventMap>(target: SVGElement, type: K, listener: (this: SVGElement, ev: SVGElementEventMap[K]) => void, options?: boolean | AddEventListenerOptions): void;
	event<K extends keyof DocumentEventMap>(target: Document, type: K, listener: (this: Document, ev: DocumentEventMap[K]) => void, options?: boolean | AddEventListenerOptions): void;
	event<K extends keyof WindowEventMap>(target: Window, type: K, listener: (this: Window, ev: WindowEventMap[K]) => void, options?: boolean | AddEventListenerOptions): void;
	event<K extends keyof WebSocketEventMap>(target: WebSocket, type: K, listener: (this: WebSocket, ev: WebSocketEventMap[K]) => void, options?: boolean | AddEventListenerOptions): void;
	event<K extends keyof XMLHttpRequestEventMap>(target: XMLHttpRequest, type: K, listener: (this: XMLHttpRequest, ev: XMLHttpRequestEventMap[K]) => void, options?: boolean | AddEventListenerOptions): void;
	event<K extends "change">(target: MediaQueryList, type: K, listener: (this: MediaQueryList, ev: MediaQueryListEventMap[K]) => void, options?: boolean | AddEventListenerOptions): void;
	event<K extends keyof AnimationEventMap>(target: Animation, type: K, listener: (this: Animation, ev: AnimationEventMap[K]) => void, options?: boolean | AddEventListenerOptions): void;
	event<K extends keyof EventSourceEventMap>(target: EventSource, type: K, listener: (this: EventSource, ev: EventSourceEventMap[K]) => void, options?: boolean | AddEventListenerOptions): void;
	event(target: EventTarget, type: string, listener: EventListenerOrEventListenerObject, options?: boolean | AddEventListenerOptions): void;
	get<T>(signal: Getter<T>): T;
	getAll<S extends readonly Getter<unknown>[]>(signals: [...S[]]): { [K in string | number | symbol]: Exclude<GetterType<S[K]>, false | "" | 0 | null | undefined> } | undefined;
	interval(fn: () => void, period: Nano): void;
	proxy<T>(dst: Setter<T>, src: Getter<T>): void;
	race<const T extends readonly unknown[]>(...values: T): Promise<Awaited<T[number]> | undefined>;
	run(fn: (effect: Effect) => void): Dispose;
	set<S extends Setter<unknown>>(signal: S, value: SetterType<S>, ...args: undefined extends SetterType<S> ? [cleanup?: SetterType<S>] : [cleanup: SetterType<S>]): void;
	spawn(fn: () => Promise<void>): void;
	subscribe<T>(signal: Getter<T>, fn: (value: T) => void): void;
	timeout(fn: (effect: Effect) => void, duration: Nano): void;
	timer(fn: () => void, duration: Nano): void;
}

export interface GetPromise<T> extends Getter<T | undefined>, PromiseLike<T> {}

export interface Getter<T> {
	changed(): Promise<T>;
	changed(fn: Subscriber<T>): Dispose;
	peek(): T;
	subscribe(fn: Subscriber<T>): Dispose;
}

export type GetterInit<T> = T | Getter<T>;

export type GetterType<G> = G extends Getter<infer T> ? T : never;

export type GetterValues<S extends readonly Getter<unknown>[]> = { [K in keyof S]: GetterType<S[K]> };

export type Inputs<I extends SignalMap> = { [K in keyof I]?: GetterInit<GetterType<I[K]>> };

export class Once<T> implements GetPromise<T> {
	constructor();
	readonly [GETTER_BRAND]: true;
	changed(): Promise<T | undefined>;
	changed(fn: (value: T | undefined) => void): Dispose;
	peek(): T | undefined;
	set(value: T): void;
	subscribe(fn: (value: T | undefined) => void): Dispose;
	then<R1 = T, R2 = never>(onFulfilled?: ((value: T) => R1 | PromiseLike<R1>) | null, onRejected?: ((reason: unknown) => R2 | PromiseLike<R2>) | null): PromiseLike<R1 | R2>;
}

export type Readonlys<T extends SignalMap> = { readonly [K in keyof T]: Getter<GetterType<T[K]>> };

export interface Setter<T> {
	set(value: T): void;
	update(fn: (prev: T) => T): void;
}

export type SetterType<S> = S extends Setter<infer T> ? T : never;

export class Signal<T> implements Getter<T>, Setter<T> {
	constructor(value: T);
	readonly [GETTER_BRAND]: true;
	readonly [SIGNAL_BRAND]: true;
	changed(): Promise<T>;
	changed(fn: Subscriber<T>): Dispose;
	static from<T>(value: T | Signal<T>): Signal<T>;
	mutate<R>(fn: (value: T) => R, notify?: boolean): R;
	peek(): T;
	static race<T extends readonly unknown[]>(...sigs: { [K in string | number | symbol]: Getter<T[K]> }): GetPromise<Awaited<T[number]>>;
	set(value: T, notify?: boolean): void;
	subscribe(fn: Subscriber<T>): Dispose;
	update(fn: (prev: T) => T, notify?: boolean): void;
	watch(fn: Subscriber<T>): Dispose;
}

export type SignalMap = Record<string, Getter<unknown>>;

export type Subscriber<T> = (value: T) => void;

export function getter<T>(value: GetterInit<T>): Getter<T>;

export function race<const T extends readonly unknown[]>(values: T): Promise<Awaited<T[number]>>;

export function readonlys<T extends SignalMap>(signals: T): Readonlys<T>;
```
