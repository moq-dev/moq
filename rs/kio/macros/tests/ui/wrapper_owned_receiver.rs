// An owned receiver, which every poll would consume.
use std::task::Poll;

struct S(kio::Queue<u8>);

impl S {
	#[kio::wrapper]
	fn poll_pop(self, waiter: &kio::Waiter) -> Poll<Result<u8, kio::Closed>> {
		self.0.poll_pop(waiter)
	}
}

fn main() {}
