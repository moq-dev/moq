// An argument that is neither `Copy` nor a reference: every poll would move it.
use std::task::Poll;

struct S;

impl S {
	#[kio::wrapper]
	fn poll_len(&self, _waiter: &kio::Waiter, name: String) -> Poll<usize> {
		Poll::Ready(name.len())
	}
}

fn main() {}
