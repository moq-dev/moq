import { Match, Switch } from "solid-js";
import useWatchUIContext from "../hooks/use-watch-ui";

export default function WatchStatusIndicator() {
	const context = useWatchUIContext();

	return (
		<output>
			<Switch>
				<Match when={context.watchStatus() === "no-url"}>🔴 No URL</Match>
				<Match when={context.watchStatus() === "disconnected"}>🔴 Disconnected</Match>
				<Match when={context.watchStatus() === "connecting"}>🟡 Connecting...</Match>
				<Match when={context.watchStatus() === "offline"}>🔴 Offline</Match>
				<Match when={context.watchStatus() === "loading"}>🟡 Loading...</Match>
				<Match when={context.watchStatus() === "live"}>🟢 Live</Match>
				<Match when={context.watchStatus() === "connected"}>🟢 Connected</Match>
			</Switch>
		</output>
	);
}
