import { For, type JSX } from "solid-js";
import * as Icon from "../../icon/icon";
import type { KnownStatsProviders, ProviderProps } from "../types";
import { StatsItem } from "./StatsItem";

/**
 * Props for stats panel component
 */
interface StatsPanelProps extends ProviderProps {}

export const statsDetailItems: { name: string; statProvider: KnownStatsProviders; icon: () => JSX.Element }[] = [
	{
		name: "Network",
		statProvider: "network",
		icon: () => <Icon.Network />,
	},
	{
		name: "Video",
		statProvider: "video",
		icon: () => <Icon.Video />,
	},
	{
		name: "Audio",
		statProvider: "audio",
		icon: () => <Icon.Audio />,
	},
	{
		name: "Buffer",
		statProvider: "buffer",
		icon: () => <Icon.Buffer />,
	},
];

/**
 * Panel displaying all metrics in a grid layout
 */
export const StatsPanel = (props: StatsPanelProps) => {
	return (
		<div class="stats__panel">
			<For each={statsDetailItems}>
				{({ name, statProvider, icon }) => (
					<StatsItem
						name={name}
						statProvider={statProvider}
						svg={icon()}
						audio={props.audio}
						video={props.video}
					/>
				)}
			</For>
		</div>
	);
};
