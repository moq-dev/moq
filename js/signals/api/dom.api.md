# `@moq/signals/dom`

```ts
import { Effect } from "@moq/signals";

export interface ArrayElement extends Array<Element> {
	[n: number]: T;
}

export type CreateOptions<T extends HTMLElement> = {
	attributes?: Record<string, string>;
	classList?: string[];
	className?: string;
	dataset?: Record<string, string>;
	id?: string;
	style?: Partial<CSSStyleDeclaration>;
} & Partial<Omit<T, "style" | "className" | "classList" | "id" | "dataset" | "attributes">>;

export type Element = Node | ArrayElement | string & {} | number | boolean | null | undefined;

export function create<K extends keyof HTMLElementTagNameMap>(tagName: K, options?: CreateOptions<HTMLElementTagNameMap[K] & HTMLElement>, ...children: (HTMLElement | string)[]): HTMLElementTagNameMap[K];

export function render(effect: Effect, parent: Node, element: Element | ((effect: Effect) => Element)): void;

export function setClass(effect: Effect, element: HTMLElement, ...classNames: string[]): void;
```
