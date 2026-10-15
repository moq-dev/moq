// Package-private: consumers use ambient functions or Manual, not backend injection.
export type Backend = {
	now(): number;
	wall(): number;
	arm(at: number, ready: () => void): () => void;
};
