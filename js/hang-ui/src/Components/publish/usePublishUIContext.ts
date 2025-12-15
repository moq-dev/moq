import { useContext } from "solid-js";
import { PublishUIContext } from "./PublishUIContextProvider";

export default function usePublishUIContext() {
	const context = useContext(PublishUIContext);

	if (!context) {
		throw new Error("usePublishUIContext must be used within a PublishUIContextProvider");
	}

	return context;
}
