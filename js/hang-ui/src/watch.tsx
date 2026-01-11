import type HangWatch from "@moq/hang/watch/element";
import { customElement } from "solid-element";
import { createSignal, onMount, Show } from "solid-js";
import { WatchUI } from "./Components/watch/index.tsx";

customElement("hang-watch-ui", (_, { element }) => {
	const [nested, setNested] = createSignal<HangWatch | undefined>();

	onMount(async () => {
		await customElements.whenDefined("hang-watch");
		const watchEl = element.querySelector("hang-watch");
		setNested(watchEl as HangWatch);
	});

	return (
		<Show when={nested()} keyed>
			{(watch: HangWatch) => <WatchUI watch={watch} />}
		</Show>
	);
});
