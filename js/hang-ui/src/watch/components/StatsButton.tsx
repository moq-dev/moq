import { Button, Icon } from "@moq/ui-core";
import useWatchUIContext from "../hooks/use-watch-ui";

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
			<Icon.Stats />
		</Button>
	);
}
