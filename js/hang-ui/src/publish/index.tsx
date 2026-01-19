import type HangPublish from "@moq/hang/publish/element";
import { customElement } from "solid-element";
import { createSignal, onMount } from "solid-js";
import { Show } from "solid-js/web";
import { PublishUI } from "./element.tsx";

customElement("hang-publish-ui", (_, { element }) => {
	const [nested, setNested] = createSignal<HangPublish | undefined>();

	onMount(async () => {
		await customElements.whenDefined("hang-publish");
		const publishEl = element.querySelector("hang-publish");
		setNested(publishEl ? (publishEl as HangPublish) : undefined);
	});

	return (
		<Show when={nested()} keyed>
			{(publish: HangPublish) => <PublishUI publish={publish} />}
		</Show>
	);
});
