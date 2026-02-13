import type HangWatch from "@moq/hang/watch/element";
import { useContext } from "solid-js";
import { Show } from "solid-js/web";
import { Stats } from "../shared/components/stats";

import BufferingIndicator from "./components/BufferingIndicator";
import WatchControls from "./components/WatchControls";
import WatchUIContextProvider, { WatchUIContext } from "./context";

export function WatchUI(props: { watch: HangWatch }) {
	return (
		<WatchUIContextProvider hangWatch={props.watch}>
			<div class="watchVideoContainer">
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
