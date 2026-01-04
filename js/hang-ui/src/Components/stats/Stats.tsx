import { type Context, Show, useContext } from "solid-js";
import { StatsPanel } from "./components/StatsPanel";
import type { ProviderProps } from "./types";

interface StatsProps<T = unknown> {
	context: Context<T>;
	getElement: (ctx: T) => ProviderProps | undefined;
}

/**
 * Stats component for displaying real-time media streaming metrics
 * Accepts a generic context and a function to extract the media element
 */
export const Stats = <T = unknown>(props: StatsProps<T>) => {
	const contextValue = useContext(props.context);

	return (
		<Show when={props.getElement(contextValue)}>
			{(_element) => (
				<div class="stats">
					<StatsPanel audio={_element().audio} video={_element().video} />
				</div>
			)}
		</Show>
	);
};
