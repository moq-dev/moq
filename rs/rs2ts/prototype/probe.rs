//! Semantic probes for a Rust-to-TypeScript runtime, independent of moq-net.

use std::{cell::RefCell, rc::Rc};

/// Add two unsigned integers, refusing overflow.
pub fn add(left: u64, right: u64) -> Option<u64> {
	left.checked_add(right)
}

/// Subtract two unsigned integers, refusing underflow.
pub fn sub(left: u64, right: u64) -> Option<u64> {
	left.checked_sub(right)
}

/// Map a signed integer onto its zigzag wire value.
pub fn zigzag(value: i64) -> u64 {
	((value << 1) ^ (value >> 63)) as u64
}

/// Recover a signed integer from a zigzag wire value.
pub fn unzigzag(value: u64) -> i64 {
	((value >> 1) as i64) ^ -((value & 1) as i64)
}

/// Distinguish every state of a nested optional integer.
pub fn nested(value: Option<Option<u64>>) -> u8 {
	match value {
		None => 0,
		Some(None) => 1,
		Some(Some(_)) => 2,
	}
}

struct Event {
	id: u8,
	events: Rc<RefCell<Vec<u8>>>,
}

impl Drop for Event {
	fn drop(&mut self) {
		self.events.borrow_mut().push(self.id);
	}
}

/// Exercise clone, move, early return, and reverse lexical drop order.
pub fn drops(early: bool) -> Vec<u8> {
	let events = Rc::new(RefCell::new(Vec::new()));
	{
		let first = Rc::new(Event {
			id: 1,
			events: events.clone(),
		});
		let second = Rc::new(Event {
			id: 2,
			events: events.clone(),
		});
		let cloned = first.clone();
		drop(first);
		let moved = cloned;
		drop(second);
		if early {
			drop(moved);
			return events.borrow().clone();
		}
		let _third = Event {
			id: 3,
			events: events.clone(),
		};
		let _fourth = Event {
			id: 4,
			events: events.clone(),
		};
	}
	let result = events.borrow().clone();
	result
}

/// Observe lexical cleanup when a scope unwinds.
pub fn unwind() -> Vec<u8> {
	let events = Rc::new(RefCell::new(Vec::new()));
	let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		let _first = Event {
			id: 1,
			events: events.clone(),
		};
		let _second = Event {
			id: 2,
			events: events.clone(),
		};
		panic!("probe");
	}));
	assert!(result.is_err());
	let result = events.borrow().clone();
	result
}

#[derive(Clone, Copy)]
struct Value {
	count: u32,
}

/// Copying a struct cannot make later mutation alias the original.
pub fn copies() -> (u32, u32) {
	let original = Value { count: 7 };
	let mut copy = original;
	copy.count += 1;
	(original.count, copy.count)
}

fn main() {
	let mut values = vec![
		0,
		1,
		(1 << 32) - 1,
		1 << 32,
		(1 << 53) - 1,
		1 << 53,
		(1 << 63) - 1,
		1 << 63,
		u64::MAX,
	];
	let mut seed = 0x6a09_e667_f3bc_c909_u64;
	for _ in 0..256 {
		seed ^= seed << 13;
		seed ^= seed >> 7;
		seed ^= seed << 17;
		values.push(seed);
	}
	for &left in &values {
		for &right in &values[..9] {
			println!(
				"add {left} {right} {}",
				add(left, right).map_or_else(|| "overflow".into(), |v| v.to_string())
			);
			println!(
				"sub {left} {right} {}",
				sub(left, right).map_or_else(|| "overflow".into(), |v| v.to_string())
			);
			println!("xor {left} {right} {}", left ^ right);
			println!("and {left} {right} {}", left & right);
		}
		let signed = left as i64;
		for shift in [0, 1, 7, 31, 32, 33, 63] {
			println!("shl {left} {shift} {}", left << shift);
			println!("shr {left} {shift} {}", left >> shift);
			println!("sar {signed} {shift} {}", signed >> shift);
		}
		println!("neg {signed} {}", signed.wrapping_neg());
		println!("zigzag {signed} {}", zigzag(signed));
		println!("unzigzag {left} {}", unzigzag(left));
	}
	println!(
		"nested {} {} {}",
		nested(None),
		nested(Some(None)),
		nested(Some(Some(u64::MAX)))
	);
	println!("drops {:?} {:?}", drops(false), drops(true));
	std::panic::set_hook(Box::new(|_| {}));
	println!("unwind {:?}", unwind());
	println!("copies {:?}", copies());
}
