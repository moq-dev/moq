import { Button, Icon } from "@moq/ui-core";
import { Show } from "solid-js";
import useWatchUIContext from "../hooks/use-watch-ui";

export default function PlayPauseButton() {
	const context = useWatchUIContext();
	const onClick = () => {
		context.togglePlayback();
	};

	return (
		<Button title={context.isPlaying() ? "Pause" : "Play"} class="button--playback" onClick={onClick}>
			<Show when={context.isPlaying()} fallback={<Icon.Play />}>
				<Icon.Pause />
			</Show>
		</Button>
	);
}
