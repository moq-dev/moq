import { customElement } from "solid-element";
import { createSignal, onMount, Show } from "solid-js";
import type MoqBoy from "../element";
import { BoyUI } from "./element.tsx";

customElement("moq-boy-ui", (_, { element }) => {
	const [nested, setNested] = createSignal<MoqBoy | undefined>();

	onMount(async () => {
		await customElements.whenDefined("moq-boy");
		const el = element.querySelector("moq-boy");
		setNested(el ? (el as MoqBoy) : undefined);
	});

	return (
		<Show when={nested()} keyed>
			{(boy: MoqBoy) => <BoyUI boy={boy} />}
		</Show>
	);
});

declare global {
	interface HTMLElementTagNameMap {
		"moq-boy-ui": HTMLElement;
	}
}
