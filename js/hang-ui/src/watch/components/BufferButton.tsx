import Button from "../shared/button";
import * as Icon from "../shared/icon";
import useWatchUIContext from "./useWatchUIContext";

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
