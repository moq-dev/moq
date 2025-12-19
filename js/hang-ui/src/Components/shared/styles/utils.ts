/**
 * Attaches a Constructable CSSStyleSheet to the root node (ShadowRoot or Document) of the given element.
 * If the element is not defined, does nothing.
 * Only works in browsers that support adoptedStyleSheets (all modern browsers).
 *
 * @param ref - The HTMLElement whose root node will receive the stylesheet.
 * @param sheet - The CSSStyleSheet to attach.
 * @returns void
 */
export function useConstructableStyle(ref: HTMLElement | undefined, sheet: CSSStyleSheet): void {
	if (!ref) return;
	const root = ref.getRootNode() as ShadowRoot | Document;
	if ("adoptedStyleSheets" in root && !root.adoptedStyleSheets.includes(sheet)) {
		root.adoptedStyleSheets = [...root.adoptedStyleSheets, sheet];
	}
}
