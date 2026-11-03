// Run-once code returns a turbofished `Poll::Pending`, which is the variant, not a type.
#![allow(dead_code)]

#[allow(unused_imports, reason = "a rejected impl expands to nothing")]
use std::task::Poll;

struct M {
	values: kio::Queue<u64>,
	state: MState,
}

#[kio::machine(self.state: MState)]
impl M {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, kio::Closed>> {
		if self.values.is_closed() {
			return Poll::Pending::<Result<u64, kio::Closed>>;
		}
		let a: u64 = wait!(A => self.values.poll_pop(waiter))?;
		Poll::Ready(Ok(a))
	}
}

fn main() {}
