import { customElement } from "solid-element";
import { createSignal, onMount } from "solid-js";
import { Show } from "solid-js/web";
import type MoqPublish from "../element";
import { PublishUI } from "./element.tsx";

customElement("moq-publish-ui", (_, { element }) => {
	const [nested, setNested] = createSignal<MoqPublish | undefined>();

	onMount(async () => {
		await customElements.whenDefined("moq-publish");
		const publishEl = element.querySelector("moq-publish");
		setNested(publishEl ? (publishEl as MoqPublish) : undefined);
	});

	return (
		<Show when={nested()} keyed>
			{(publish: MoqPublish) => <PublishUI publish={publish} />}
		</Show>
	);
});

declare global {
	interface HTMLElementTagNameMap {
		"moq-publish-ui": HTMLElement;
	}
}
