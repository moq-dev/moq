import { For } from "solid-js";
import { PANEL_SVGS } from "./icons";
import { StatsItem } from "./StatsItem";
import type { Icons, HandlerProps } from "../types";

interface StatsPanelProps extends HandlerProps {}

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
