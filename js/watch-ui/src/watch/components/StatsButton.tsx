import Button from "../../shared/components/button/button";
import * as Icon from "../../shared/components/icon/icon";
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
