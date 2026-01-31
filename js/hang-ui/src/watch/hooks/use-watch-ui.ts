import { useContext } from "solid-js";
import { WatchUIContext, type WatchUIContextValues } from "../context";

export default function useWatchUIContext(): WatchUIContextValues {
	const context = useContext(WatchUIContext);

	if (!context) {
		throw new Error("useWatchUIContext must be used within a WatchUIContextProvider");
	}

	return context;
}
