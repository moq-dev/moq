import { Show } from "solid-js";
import { Button, Icon } from "@moq/ui-core";
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
