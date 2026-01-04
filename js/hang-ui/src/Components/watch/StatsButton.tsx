import Button from "../shared/components/button";
import Icon from "../shared/components/icon";
import useWatchUIContext from "./useWatchUIContext";

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
			<Icon name="stats" />
		</Button>
	);
}
