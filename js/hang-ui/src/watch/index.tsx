import type HangWatch from "@moq/hang/watch/element";
import { render } from "solid-js/web";
import { WatchUI } from "./element.tsx";
import styles from "./styles/index.css?inline";

/**
 * @tag hang-watch-ui
 * @summary Watch video stream with full UI controls
 * @description A custom element that provides a complete user interface for watching live or on-demand video streams
 * over Media over QUIC (MOQ). Includes playback controls, quality selection, stats panel, and automatic buffering.
 *
 * @slot default - Container for the hang-watch element and canvas
 *
 * @example HTML
 * ```html
 * <hang-watch-ui>
 *   <hang-watch url="https://example.com/stream" path="/stream" jitter="100" muted reload volume="0">
 *     <canvas style="width: 100%; height: auto;" width="1280" height="720"></canvas>
 *   </hang-watch>
 * </hang-watch-ui>
 * ```
 *
 * @example React
 * ```tsx
 * import '@moq/hang/watch/element';
 * import '@moq/hang-ui/watch';
 * import { HangWatchUI } from '@moq/hang-ui/react';
 *
 * export function HangWatchComponent({ url, path }) {
 *   return (
 *     <HangWatchUI>
 *       <hang-watch url={url} path={path} jitter="100" muted reload volume="0">
 *         <canvas style={{ width: '100%', height: 'auto' }} width="1280" height="720" />
 *       </hang-watch>
 *     </HangWatchUI>
 *   );
 * }
 * ```
 */
class HangWatchComponent extends HTMLElement {
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

			const watch = this.querySelector("hang-watch") as HangWatch | null;
			this.#dispose = render(
				() => (
					<>
						<style>{styles}</style>
						<slot />
						{watch ? <WatchUI watch={watch} /> : null}
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

customElements.define("hang-watch-ui", HangWatchComponent);
export { HangWatchComponent };
