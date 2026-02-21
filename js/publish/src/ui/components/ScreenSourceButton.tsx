import { Button, Icon } from "@moq/ui-core";
import usePublishUIContext from "../hooks/use-publish-ui";

export default function ScreenSourceButton() {
	const context = usePublishUIContext();
	const onClick = () => {
		context.moqPublish.source = "screen";
		context.moqPublish.invisible = false;
		context.moqPublish.muted = true;
	};

	return (
		<Button
			title="Screen"
			class={`publish-ui__source-button flex--center ${context.screenActive() ? "publish-ui__source-button--active" : ""}`}
			onClick={onClick}
		>
			<Icon.Screen />
		</Button>
	);
}
