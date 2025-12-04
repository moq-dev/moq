import { For } from "solid-js";
import { PANEL_SVGS } from "./icons";
import { StatsItem } from "./StatsItem";
import type { Icons, HandlerProps } from "../types";

/**
 * Props for stats panel component
 */
interface StatsPanelProps extends HandlerProps {}

/**
 * Panel displaying all metrics in a grid layout
 */
export const StatsPanel = (props: StatsPanelProps) => {
	return (
		<div class="stats__panel">
			<For each={Object.entries(PANEL_SVGS)}>
				{([icon, svg]) => (
					<StatsItem 
						icon={icon as Icons} 
						svg={svg} 
						audio={props.audio}
						video={props.video}
					/>
				)}
			</For>
		</div>
	);
};
