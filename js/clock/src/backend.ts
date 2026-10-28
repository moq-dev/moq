// Package-private: consumers use ambient functions or Manual, not backend injection.
export type Backend = {
	now(): number;
	wall(): number;
	// The wall time of instant zero. It must not jitter between reads, so independent stamps agree.
	origin(): number;
	// `ref: false` must not keep the host process alive.
	arm(at: number, ready: () => void, ref: boolean): () => void;
};
