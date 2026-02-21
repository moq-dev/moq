import { Stats } from "@moq/ui-core";
import { useContext } from "solid-js";
import { Show } from "solid-js/web";
import type MoqWatch from "../element";

import BufferingIndicator from "./components/BufferingIndicator";
import WatchControls from "./components/WatchControls";
import WatchUIContextProvider, { WatchUIContext } from "./context";
import styles from "./styles/index.css?inline";

export function WatchUI(props: { watch: MoqWatch }) {
	return (
		<WatchUIContextProvider moqWatch={props.watch}>
			<style>{styles}</style>
			<div class="watch-ui__video-container">
				<slot />
				{(() => {
					const context = useContext(WatchUIContext);
					if (!context) return null;
					return (
						<Show when={context.isStatsPanelVisible()}>
							<Stats
								context={WatchUIContext}
								getElement={(ctx) => {
									return ctx?.moqWatch.backend;
								}}
							/>
						</Show>
					);
				})()}
				<BufferingIndicator />
			</div>
			<WatchControls />
		</WatchUIContextProvider>
	);
}
