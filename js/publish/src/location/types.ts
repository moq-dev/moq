export type Position = {
	x?: number;
	y?: number;
	z?: number;
	s?: number;
};

export type Peers = Record<string, Position>;
