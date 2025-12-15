import { useContext } from "solid-js";
import { WatchUIContext } from "./WatchUIContextProvider";

export default function useWatchUIContext() {
	const context = useContext(WatchUIContext);

	if (!context) {
		throw new Error("useWatchUIContext must be used within a WatchUIContextProvider");
	}

	return context;
}
