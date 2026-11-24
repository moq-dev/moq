import { useContext } from "solid-js";
import { BoyUIContext, type BoyUIContextValues, GameUIContext, type GameUIContextValues } from "../context";

export function useBoyUI(): BoyUIContextValues {
	const context = useContext(BoyUIContext);
	if (!context) {
		throw new Error("useBoyUI must be used within a BoyUIContextProvider");
	}
	return context;
}

export function useGameUI(): GameUIContextValues {
	const context = useContext(GameUIContext);
	if (!context) {
		throw new Error("useGameUI must be used within a GameUIContextProvider");
	}
	return context;
}
