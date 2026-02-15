import { customElement } from "solid-element";
import { createSignal, onMount, Show } from "solid-js";
import type MoqWatch from "../element";
import { WatchUI } from "./element.tsx";

customElement("moq-watch-ui", (_, { element }) => {
	const [nested, setNested] = createSignal<MoqWatch | undefined>();

	onMount(async () => {
		await customElements.whenDefined("moq-watch");
		const watchEl = element.querySelector("moq-watch");
		setNested(watchEl ? (watchEl as MoqWatch) : undefined);
	});

	return (
		<Show when={nested()} keyed>
			{(watch: MoqWatch) => <WatchUI watch={watch} />}
		</Show>
	);
});

declare global {
	interface HTMLElementTagNameMap {
		"moq-watch-ui": HTMLElement;
	}
}
