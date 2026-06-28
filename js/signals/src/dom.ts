/**
 * DOM helpers built on Effects: create elements, render reactive content, and
 * toggle classes with automatic cleanup when the owning effect tears down.
 *
 * @module
 */

import type { Effect } from ".";

/** Options for {@link create}: styles, classes, dataset, attributes, plus any element properties. */
export type CreateOptions<T extends HTMLElement> = {
	style?: Partial<CSSStyleDeclaration>;
	className?: string;
	classList?: string[];
	id?: string;
	dataset?: Record<string, string>;
	attributes?: Record<string, string>;
} & Partial<Omit<T, "style" | "className" | "classList" | "id" | "dataset" | "attributes">>;

/** Creates an HTML element, applying the given options and appending the given children. */
export function create<K extends keyof HTMLElementTagNameMap>(
	tagName: K,
	options?: CreateOptions<HTMLElementTagNameMap[K] & HTMLElement>,
	...children: (HTMLElement | string)[]
): HTMLElementTagNameMap[K] {
	const element = document.createElement(tagName);

	if (!options) return element;

	const { style, classList, dataset, attributes, ...props } = options;

	// Apply styles
	if (style) {
		Object.assign(element.style, style);
	}

	// Apply class list
	if (classList) {
		element.classList.add(...classList);
	}

	// Apply dataset
	if (dataset) {
		Object.entries(dataset).forEach(([key, value]) => {
			element.dataset[key] = value;
		});
	}

	// Apply attributes
	if (attributes) {
		Object.entries(attributes).forEach(([key, value]) => {
			element.setAttribute(key, value);
		});
	}

	// Append children
	if (children) {
		children.forEach((child) => {
			if (typeof child === "string") {
				element.appendChild(document.createTextNode(child));
			} else {
				element.appendChild(child);
			}
		});
	}

	// Apply other properties
	Object.assign(element, props);

	return element;
}

/** Renderable content: a node, array of nodes, primitive, or nullish. Matches solid.js's JSX.Element. */
export type Element = Node | ArrayElement | (string & {}) | number | boolean | null | undefined;
interface ArrayElement extends Array<Element> {}

/** Renders `element` into `parent`, removing it again when the effect reruns or closes. */
export function render(effect: Effect, parent: Node, element: Element | ((effect: Effect) => Element)) {
	const e = typeof element === "function" ? element(effect) : element;
	if (e === undefined || e === null) return;

	let node: Node;
	if (e instanceof Node) {
		node = e;
	} else if (Array.isArray(e)) {
		node = document.createDocumentFragment();
		for (const child of e) {
			render(effect, node, child);
		}
	} else if (typeof e === "number" || typeof e === "boolean" || typeof e === "string") {
		node = document.createTextNode(e.toString());
	} else {
		const exhaustive: never = e;
		throw new Error(`Invalid element type: ${exhaustive}`);
	}

	parent.appendChild(node);
	effect.cleanup(() => parent.removeChild(node));
}

/** Adds the given classes to `element`, removing them again when the effect reruns or closes. */
export function setClass(effect: Effect, element: HTMLElement, ...classNames: string[]) {
	for (const className of classNames) {
		element.classList.add(className);
	}

	effect.cleanup(() => {
		for (const className of classNames) {
			element.classList.remove(className);
		}
	});
}
