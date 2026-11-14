import { For, Show } from "solid-js";
import { render } from "solid-js/web";
import type MoqBoy from "../element";
import GameCard from "./components/GameCard";
import BoyUIContextProvider from "./context";
import { useBoyUI } from "./hooks/use-boy-ui";
import styles from "./styles/index.css?inline";

/** Render the UI for `boy` into `root`, returning the function that removes it. */
export function mount(boy: MoqBoy, root: ShadowRoot): () => void {
	return render(() => <BoyUI boy={boy} />, root);
}

function BoyUI(props: { boy: MoqBoy }) {
	return (
		<BoyUIContextProvider boy={props.boy}>
			<style>{styles}</style>
			<BoyUIContent />
		</BoyUIContextProvider>
	);
}

function BoyUIContent() {
	const ctx = useBoyUI();

	const gameList = () => [...ctx.games().values()];
	const isEmpty = () => ctx.games().size === 0;
	const isConnecting = () => ctx.connectionStatus() === "connecting";
	const isDisconnected = () => ctx.connectionStatus() === "disconnected";

	return (
		<div class="boy__grid" classList={{ "boy__grid--expanded": ctx.expanded() !== undefined }}>
			<Show when={!isDisconnected() && isEmpty()}>
				<div class="boy__empty">
					<Show when={isConnecting()} fallback={<span>No games found</span>}>
						<span>Connecting...</span>
					</Show>
				</div>
			</Show>
			<For each={gameList()}>{(game) => <GameCard game={game} />}</For>
		</div>
	);
}
