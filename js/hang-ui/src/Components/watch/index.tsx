import type HangWatch from "@moq/hang/watch/element";
import { useContext } from "solid-js";
import { Show } from "solid-js/web";
import { Stats } from "../stats";
import BufferingIndicator from "./BufferingIndicator";
import styles from "./index.css?inline";
import WatchControls from "./WatchControls";
import WatchUIContextProvider, { WatchUIContext } from "./WatchUIContextProvider";

export function WatchUI(props: { watch: HangWatch }) {
	return (
		<WatchUIContextProvider hangWatch={props.watch}>
			<style>{styles}</style>
			<div class="watchVideoContainer">
				<slot />
				{(() => {
					const context = useContext(WatchUIContext);
					if (!context) return null;
					return (
						<Show when={context.isStatsPanelVisible()}>
							<Stats
								context={WatchUIContext}
								getElement={(ctx): HangWatch | undefined => {
									return ctx?.hangWatch;
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
