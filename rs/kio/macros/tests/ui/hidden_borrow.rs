// A borrow behind an elided path lifetime: a known limit, reported as a missing lifetime.
#![allow(dead_code)]

#[allow(unused_imports, reason = "a rejected impl expands to nothing")]
use std::task::Poll;

struct M {
	values: kio::Queue<u64>,
	cell: std::cell::RefCell<u64>,
	state: MState,
}

#[kio::machine(self.state: MState)]
impl M {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, kio::Closed>> {
		let a: u64 = wait!(A => self.values.poll_pop(waiter))?;
		let label: std::cell::Ref<u64> = self.cell.borrow();
		let b: u64 = wait!(B => self.values.poll_pop(waiter))?;
		Poll::Ready(Ok(*label + a + b))
	}
}

fn main() {}
