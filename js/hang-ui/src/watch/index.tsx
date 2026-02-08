import type HangWatch from "@moq/hang/watch/element";
import { customElement } from "solid-element";
import { createSignal, onMount, Show } from "solid-js";
import { WatchUI } from "./element.tsx";

customElement("hang-watch-ui", (_, { element }) => {
	const [nested, setNested] = createSignal<HangWatch | undefined>();

	onMount(async () => {
		await customElements.whenDefined("hang-watch");
		const watchEl = element.querySelector("hang-watch");
		setNested(watchEl ? (watchEl as HangWatch) : undefined);
	});

	return (
		<Show when={nested()} keyed>
			{(watch: HangWatch) => <WatchUI watch={watch} />}
		</Show>
	);
});
