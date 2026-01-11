import type HangPublish from "@moq/hang/publish/element";
import styles from "./index.css?inline";
import PublishControls from "./PublishControls";
import PublishControlsContextProvider from "./PublishUIContextProvider";

export function PublishUI(props: { publish: HangPublish }) {
	return (
		<>
			<style>{styles}</style>
			<slot></slot>
			<PublishControlsContextProvider hangPublish={props.publish}>
				<PublishControls />
			</PublishControlsContextProvider>
		</>
	);
}
