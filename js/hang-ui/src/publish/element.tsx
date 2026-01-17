import type HangPublish from "@moq/hang/publish/element";
import PublishControls from "./components/PublishControls";
import PublishControlsContextProvider from "./context";
import styles from "./styles/index.css?inline";

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
