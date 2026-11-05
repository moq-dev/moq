// A wait's poll moves a stored local, which it only borrows.
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
		let label: String = format!("m-{a}");
		let b: u64 = wait!(B => {
			drop(label);
			self.values.poll_pop(waiter)
		})?;
		Poll::Ready(Ok(label.len() as u64 + a + b))
	}
}

fn main() {}
