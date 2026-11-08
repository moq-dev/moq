// A method without the `poll_` prefix.
use std::task::Poll;

struct S(kio::Queue<u8>);

impl S {
	#[kio::wrapper]
	fn pop(&self, waiter: &kio::Waiter) -> Poll<Result<u8, kio::Closed>> {
		self.0.poll_pop(waiter)
	}
}

fn main() {}
