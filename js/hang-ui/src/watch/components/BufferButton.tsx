import Button from "../../shared/components/button/button";
import * as Icon from "../../shared/components/icon/icon";
import useWatchUIContext from "../hooks/use-watch-ui";

/**
 * Toggle button for showing/hiding buffer overlay
 */
export default function BufferButton() {
	const context = useWatchUIContext();

	const onClick = () => {
		context.setIsBufferOverlayVisible(!context.isBufferOverlayVisible());
	};

	return (
		<Button title={context.isBufferOverlayVisible() ? "Hide buffer stats" : "Show buffer stats"} onClick={onClick}>
			<Icon.Buffer />
		</Button>
	);
}
