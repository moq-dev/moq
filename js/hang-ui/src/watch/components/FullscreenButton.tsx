import { Show } from "solid-js";
import Button from "../../shared/components/button/button";
import * as Icon from "../../shared/components/icon/icon";
import useWatchUIContext from "../hooks/use-watch-ui";

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
