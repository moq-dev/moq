import { createContext, useContext } from "solid-js";
import type { HandlerProps } from "./types";

/**
 * Context providing stream sources to child components
 */
export const StatsContext = createContext<HandlerProps>({});

/**
 * Hook to access stream sources from context
 * @returns Stream properties (audio and video)
 */
export const useMetrics = () => {
	const context = useContext(StatsContext);
	return context;
};
