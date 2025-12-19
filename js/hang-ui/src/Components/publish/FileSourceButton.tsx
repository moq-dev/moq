import { createSignal } from "solid-js";
import Button from "../shared/components/button";
import type { Icon } from "../shared/types/icons";
import usePublishUIContext from "./usePublishUIContext";

const fileIcon: Icon = () => (
	<svg
		xmlns="http://www.w3.org/2000/svg"
		width="24"
		height="24"
		viewBox="0 0 24 24"
		fill="none"
		stroke="currentColor"
		stroke-width="2"
		stroke-linecap="round"
		stroke-linejoin="round"
		aria-hidden="true"
	>
		<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
	</svg>
);

export default function FileSourceButton() {
	const [fileInputRef, setFileInputRef] = createSignal<HTMLInputElement | undefined>();
	const context = usePublishUIContext();
	const onClick = () => fileInputRef()?.click();
	const onChange = (event: Event) => {
		const castedInputEl = event.target as HTMLInputElement;
		const file = castedInputEl.files?.[0];

		if (file) {
			context.setFile(file);
			castedInputEl.value = "";
		}
	};

	return (
		<>
			<input
				ref={setFileInputRef}
				onChange={onChange}
				type="file"
				class="hidden"
				accept="video/*,audio/*,image/*"
			/>
			<Button
				title="Upload File"
				class={`publishSourceButton ${context.fileActive() ? "active" : ""}`}
				onClick={onClick}
			>
				{fileIcon()}
			</Button>
		</>
	);
}
