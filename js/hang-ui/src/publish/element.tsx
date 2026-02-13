import type HangPublish from "@moq/hang/publish/element";
import PublishControls from "./components/PublishControls";
import PublishControlsContextProvider from "./context";

export function PublishUI(props: { publish: HangPublish }) {
	return (
		<PublishControlsContextProvider hangPublish={props.publish}>
			<PublishControls />
		</PublishControlsContextProvider>
	);
}
