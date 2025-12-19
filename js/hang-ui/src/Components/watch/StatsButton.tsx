import Button from "../shared/components/button";
import type { Icon } from "../shared/types/icons";
import useWatchUIContext from "./useWatchUIContext";

const statsButtonIcon: Icon = () => (
	<svg
		xmlns="http://www.w3.org/2000/svg"
		width="24"
		height="24"
		viewBox="0 0 24 24"
		fill="none"
		stroke="currentColor"
		stroke-width="2"
		stroke-linecap="round"
		stroke-linejoin="round"
		aria-hidden="true"
		class="stats__icon"
	>
		<path d="M5 21v-6" />
		<path d="M12 21V3" />
		<path d="M19 21V9" />
	</svg>
);

/**
 * Toggle button for showing/hiding stats panel
 */
export default function StatsButton() {
	const context = useWatchUIContext();

	const onClick = () => {
		context.setIsStatsPanelVisible(!context.isStatsPanelVisible());
	};

	return (
		<Button title={context.isStatsPanelVisible() ? "Hide stats" : "Show stats"} onClick={onClick}>
			{statsButtonIcon()}
		</Button>
	);
}
