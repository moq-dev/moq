import { For } from "solid-js";
import Icon from "../../shared/components/icon";
import type { KnownStatsProviders, ProviderProps } from "../types";
import { StatsItem } from "./StatsItem";

/**
 * Props for stats panel component
 */
interface StatsPanelProps extends ProviderProps {}

export const statsDetailItems: { name: string; statProvider: KnownStatsProviders; iconName: string }[] = [
	{
		name: "Network",
		statProvider: "network",
		iconName: "network",
	},
	{
		name: "Video",
		statProvider: "video",
		iconName: "video",
	},
	{
		name: "Audio",
		statProvider: "audio",
		iconName: "audio",
	},
	{
		name: "Buffer",
		statProvider: "buffer",
		iconName: "buffer",
	},
];

/**
 * Panel displaying all metrics in a grid layout
 */
export const StatsPanel = (props: StatsPanelProps) => {
	return (
		<div class="stats__panel">
			<For each={statsDetailItems}>
				{({ name, statProvider, iconName }) => (
					<StatsItem
						name={name}
						statProvider={statProvider}
						svg={<Icon name={iconName} />}
						audio={props.audio}
						video={props.video}
					/>
				)}
			</For>
		</div>
	);
};
