import { createContext, useContext } from "solid-js";
import type { HandlerProps } from "./types";

export const StatsContext = createContext<HandlerProps>({});

export const useMetrics = () => {
	const context = useContext(StatsContext);
	return context;
};
