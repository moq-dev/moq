import { Button, Icon } from "@moq/ui-core";
import usePublishUIContext from "../hooks/use-publish-ui";

export default function NothingSourceButton() {
	const context = usePublishUIContext();
	const onClick = () => {
		context.moqPublish.source.set(undefined);
		context.moqPublish.muted.set(true);
		context.moqPublish.invisible.set(true);
	};

	return (
		<Button
			title="No Source"
			class={`publish-ui__source-button flex--center publish-ui__source-button--no-source ${context.nothingActive() ? "publish-ui__source-button--no-source-active" : ""}`}
			onClick={onClick}
		>
			<Icon.Ban />
		</Button>
	);
}
