declare global {
	interface HTMLElementTagNameMap {
		"hang-watch-ui": HTMLElement;
	}
	namespace JSX {
		interface IntrinsicElements {
			"hang-watch-ui": {
				children?: JSX.Element | JSX.Element[];
			};
		}
	}
}

declare module "react" {
	namespace JSX {
		interface IntrinsicElements {
			"hang-watch-ui": {
				children?: JSX.Element | JSX.Element[];
			};
		}
	}
}

export {};
