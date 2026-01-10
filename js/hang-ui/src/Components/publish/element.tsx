import type HangPublish from "@moq/hang/publish/element";
import { customElement } from "solid-element";
import { createSignal, onMount } from "solid-js";
import { Show } from "solid-js/web";
import PublishControls from "./PublishControls";
import PublishControlsContextProvider from "./PublishUIContextProvider";
import styles from "./styles.css?inline";

customElement("hang-publish-ui", {}, function PublishControlsWebComponent(_, { element }) {
	const [hangPublishEl, setHangPublishEl] = createSignal<HangPublish | undefined>();

	onMount(async () => {
		// Inject CSS into shadow root
		const rootElement = (element as unknown as HTMLElement).shadowRoot || (element as unknown as HTMLElement);
		const styleElement = document.createElement("style");
		styleElement.textContent = styles;
		rootElement.appendChild(styleElement);

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
