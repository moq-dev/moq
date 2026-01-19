import { Show } from "solid-js";
import Button from "../shared/button";
import * as Icon from "../shared/icon";
import useWatchUIContext from "./useWatchUIContext";

export default function FullscreenButton() {
	const context = useWatchUIContext();

	const onClick = () => {
		context.toggleFullscreen();
	};

	return (
		<Button title="Fullscreen" onClick={onClick}>
			<Show when={context.isFullscreen()} fallback={<Icon.FullscreenEnter />}>
				<Icon.FullscreenExit />
			</Show>
		</Button>
	);
}
