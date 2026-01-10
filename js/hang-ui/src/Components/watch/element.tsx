import type HangWatch from "@moq/hang/watch/element";
import { customElement } from "solid-element";
import { createSignal, onMount, useContext } from "solid-js";
import { Show } from "solid-js/web";
import { Stats } from "../stats/element";
import type { ProviderProps } from "../stats/types";
import BufferingIndicator from "./BufferingIndicator";
import styles from "./styles.css?inline";
import WatchControls from "./WatchControls";
import WatchUIContextProvider, { WatchUIContext } from "./WatchUIContextProvider";

customElement("hang-watch-ui", {}, function WatchUIWebComponent(_, { element }) {
	const [hangWatchEl, setHangWatchEl] = createSignal<HangWatch | undefined>();

	onMount(async () => {
		// Inject CSS into shadow root
		const rootElement = (element as unknown as HTMLElement).shadowRoot || (element as unknown as HTMLElement);
		const styleElement = document.createElement("style");
		styleElement.textContent = styles;
		rootElement.appendChild(styleElement);

		const watchEl = element.querySelector("hang-watch");
		await customElements.whenDefined("hang-watch");
		setHangWatchEl(watchEl);
	});

	return (
		<Show when={hangWatchEl()} keyed>
			{(watchEl: HangWatch) => (
				<WatchUIContextProvider hangWatch={watchEl}>
					<div class="watchVideoContainer">
						<slot />
						{(() => {
							const context = useContext(WatchUIContext);
							if (!context) return null;
							return (
								<Show when={context.isStatsPanelVisible()}>
									<Stats
										context={WatchUIContext}
										getElement={(ctx): ProviderProps | undefined => {
											if (!ctx?.hangWatch) return undefined;
											return {
												audio: { source: ctx.hangWatch.audio.source },
												video: { source: ctx.hangWatch.video.source },
											};
										}}
									/>
								</Show>
							);
						})()}
						<BufferingIndicator />
					</div>
					<WatchControls />
				</WatchUIContextProvider>
			)}
		</Show>
	);
});
