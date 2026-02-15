import type MoqPublish from "../element";
import PublishControls from "./components/PublishControls";
import PublishControlsContextProvider from "./context";
import styles from "./styles/index.css?inline";

export function PublishUI(props: { publish: MoqPublish }) {
	return (
		<>
			<style>{styles}</style>
			<slot></slot>
			<PublishControlsContextProvider moqPublish={props.publish}>
				<PublishControls />
			</PublishControlsContextProvider>
		</>
	);
}
