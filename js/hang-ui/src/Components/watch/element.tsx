import type HangWatch from "@moq/hang/watch/element";
import { customElement } from "solid-element";
import { createSignal, onMount } from "solid-js";
import { Show } from "solid-js/web";
import BufferingIndicator from "./BufferingIndicator";
import styles from "./styles.css?inline";
import WatchControls from "./WatchControls";
import WatchUIContextProvider from "./WatchUIContextProvider";

customElement("hang-watch-ui", {}, function WatchUIWebComponent(_, { element }) {
	const [hangWatchEl, setHangWatchEl] = createSignal<HangWatch | undefined>();

	onMount(async () => {
		const watchEl = element.querySelector("hang-watch");
		await customElements.whenDefined("hang-watch");
		setHangWatchEl(watchEl);
	});

	return (
		<Show when={hangWatchEl()} keyed>
			{(watchEl: HangWatch) => (
				<WatchUIContextProvider hangWatch={watchEl}>
					<style>{styles}</style>
					<div class="watchVideoContainer">
						<slot />
						<BufferingIndicator />
					</div>
					<WatchControls />
				</WatchUIContextProvider>
			)}
		</Show>
	);
});
