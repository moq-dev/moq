export interface ControlHintState {
	active: boolean;
	focused: boolean;
	estopped: boolean;
	estopBy?: string;
	status: "pending" | "available" | "unavailable";
	driverId?: string;
	statusDriver?: string;
	wheel: boolean;
}

/** Describe the control state without confusing a dead status track for a pending lease. */
export function controlHint(state: ControlHintState): string {
	if (state.estopped) {
		const actor = state.estopBy ? ` by ${state.estopBy}` : "";
		const action = state.focused ? "R to reset" : "Reset to resume";
		return `Stopped${actor} - ${action}`;
	}
	if (!state.active && !state.focused) return "Open camera";
	if (!state.active && state.status === "unavailable") {
		return "Control unavailable · Esc to leave";
	}
	if (!state.active && state.statusDriver) return `Driven by ${state.statusDriver}`;
	if (!state.active && state.status === "pending") return "Waiting for vehicle status";
	if (!state.active) return "Take control to drive";
	if (state.status === "unavailable") return "Vehicle status lost · Control unconfirmed";
	if (!state.driverId || !state.statusDriver) return "Requesting control…";
	if (state.statusDriver !== state.driverId) return `Driven by ${state.statusDriver}`;
	if (state.wheel) {
		return "Wheel · Brake before shifting · Esc to release";
	}
	return "W/S pedals · A/D steer · Brake + G shifts · Esc to release";
}
