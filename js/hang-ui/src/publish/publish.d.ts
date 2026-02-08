declare global {
	interface HTMLElementTagNameMap {
		"hang-publish-ui": HTMLElement;
	}
	namespace JSX {
		interface IntrinsicElements {
			"hang-publish-ui": {
				children?: JSX.Element | JSX.Element[];
			};
		}
	}
}

declare module "react" {
	namespace JSX {
		interface IntrinsicElements {
			"hang-publish-ui": {
				children?: JSX.Element | JSX.Element[];
			};
		}
	}
}

export {};
