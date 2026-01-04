import type HangPublish from "@moq/hang/publish/element";
import { customElement } from "solid-element";
import { createSignal, onMount } from "solid-js";
import { Show } from "solid-js/web";
import { loadStyleIntoShadow } from "../../utilities";
import PublishControls from "./PublishControls";
import PublishControlsContextProvider from "./PublishUIContextProvider";

customElement("hang-publish-ui", {}, function PublishControlsWebComponent(_, { element }) {
	const [hangPublishEl, setHangPublishEl] = createSignal<HangPublish | undefined>();

	onMount(async () => {
		// Load CSS into the component (loadStyleIntoShadow handles basePath internally)
		const rootElement = (element as unknown as HTMLElement).shadowRoot || (element as unknown as HTMLElement);
		await loadStyleIntoShadow("themes/publish/styles.css", rootElement);

		const publishEl = element.querySelector("hang-publish");
		await customElements.whenDefined("hang-publish");
		setHangPublishEl(publishEl);
	});

	return (
		<>
			<slot></slot>
			<Show when={hangPublishEl()} keyed>
				{(el: HangPublish) => (
					<PublishControlsContextProvider hangPublish={el}>
						<PublishControls />
					</PublishControlsContextProvider>
				)}
			</Show>
		</>
	);
});
