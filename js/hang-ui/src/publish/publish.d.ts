declare global {
	interface HTMLElementTagNameMap {
		"hang-publish-ui": HTMLElement;
	}
	namespace JSX {
		interface IntrinsicElements {
			"hang-publish-ui": HTMLElement;
		}
	}
}

declare module "react" {
	namespace JSX {
		interface IntrinsicElements {
			"hang-publish-ui": import("react").DetailedHTMLProps<
				import("react").HTMLAttributes<HTMLElement>,
				HTMLElement
			>;
		}
	}
}

export {};
