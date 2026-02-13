import type HangPublish from "@moq/hang/publish/element";
import { render } from "solid-js/web";
import { PublishUI } from "./element.tsx";
import styles from "./styles/index.css?inline";

/**
 * @tag hang-publish-ui
 * @summary Publish audio/video stream with UI controls
 * @description A custom element that provides a complete user interface for publishing live video or audio streams
 * over Media over QUIC (MOQ). Includes media source selection (camera, microphone, screen, file) and publishing controls.
 *
 * @slot default - Container for the hang-publish element
 *
 * @example HTML
 * ```html
 * <hang-publish-ui>
 *   <hang-publish url="https://example.com/relay" path="/stream">
 *   </hang-publish>
 * </hang-publish-ui>
 * ```
 *
 * @example React
 * ```tsx
 * import '@moq/hang/publish/element';
 * import '@moq/hang-ui/publish';
 * import { HangPublishUI } from '@moq/hang-ui/react';
 *
 * export function HangPublishComponent({ url, path }) {
 *   return (
 *     <HangPublishUI>
 *       <hang-publish url={url} path={path} />
 *     </HangPublishUI>
 *   );
 * }
 * ```
 */
class HangPublishComponent extends HTMLElement {
	#root?: ShadowRoot;
	#dispose?: () => void;
	#connected = false;

	connectedCallback() {
		this.#connected = true;

		// Reuse existing shadow root on reconnect (attachShadow throws if called twice)
		this.#root ??= this.attachShadow({ mode: "open" });

		// Defer render to allow frameworks to append children first
		queueMicrotask(() => {
			if (!this.#connected || !this.#root) return;

			const publish = this.querySelector("hang-publish") as HangPublish | null;
			this.#dispose = render(
				() => (
					<>
						<style>{styles}</style>
						<slot />
						{publish ? <PublishUI publish={publish} /> : null}
					</>
				),
				this.#root,
			);
		});
	}

	disconnectedCallback() {
		this.#connected = false;
		this.#dispose?.();
		this.#dispose = undefined;
	}
}

customElements.define("hang-publish-ui", HangPublishComponent);
export { HangPublishComponent };
