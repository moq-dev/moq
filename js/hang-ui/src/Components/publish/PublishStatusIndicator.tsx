import { Match, Switch } from "solid-js";
import usePublishUIContext from "./usePublishUIContext";

export default function PublishStatusIndicator() {
	const context = usePublishUIContext();

	return (
		<output>
			<Switch>
				<Match when={context.publishStatus() === "no-url"}>🔴 No URL</Match>
				<Match when={context.publishStatus() === "disconnected"}>🔴 Disconnected</Match>
				<Match when={context.publishStatus() === "connecting"}>🟡 Connecting...</Match>
				<Match when={context.publishStatus() === "select-source"}>🟡 Select Source</Match>
				<Match when={context.publishStatus() === "video-only"}>🟢 Video Only</Match>
				<Match when={context.publishStatus() === "audio-only"}>🟢 Audio Only</Match>
				<Match when={context.publishStatus() === "live"}>🟢 Live</Match>
			</Switch>
		</output>
	);
}
