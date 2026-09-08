use super::*;
use storage::Error;

#[test]
fn chat_is_small_and_empty_group_allocates_no_page() {
	for layout in [Layout::Indexed, Layout::Packed] {
		assert_eq!(Group::new(layout).footprint().page_bytes, 0);
		let group = fill(Case {
			layout,
			size: 100,
			count: 1,
			publish_every: 1,
		});
		assert_eq!(group.footprint().pages, 1);
		assert_eq!(group.footprint().page_bytes, 128);
		drain(
			&group,
			Case {
				layout,
				size: 100,
				count: 1,
				publish_every: 32,
			},
			true,
		);
	}
}

#[test]
fn pages_grow_and_frames_cross_page_boundaries() {
	for layout in [Layout::Indexed, Layout::Packed] {
		let group = fill(Case {
			layout,
			size: 100,
			count: 10000,
			publish_every: 32,
		});
		// 128 + 256 + ... + 32768 + enough 65536-byte pages.
		let bytes: usize = if matches!(layout, Layout::Packed) {
			1160000
		} else {
			1000000
		};
		let expected = 65408 + (bytes - 65408).div_ceil(65536) * 65536;
		assert_eq!(group.footprint().page_bytes, expected);
		drain(
			&group,
			Case {
				layout,
				size: 100,
				count: 10000,
				publish_every: 32,
			},
			true,
		);
	}
}

#[test]
fn live_reader_retains_slice_while_writer_grows() {
	for layout in [Layout::Indexed, Layout::Packed] {
		let mut group = Group::new(layout);
		let mut consumer = group.consumer();
		assert!(matches!(consumer.next().unwrap(), Read::Pending));
		let mut writer = group
			.create(Header {
				timestamp: 7,
				size: 200000,
			})
			.unwrap();
		writer.put_slice(b"hello");
		writer.publish();
		let Read::Ready(mut reader) = consumer.next().unwrap() else {
			panic!()
		};
		let Read::Ready(prefix) = reader.read_chunk().unwrap() else {
			panic!()
		};
		assert_eq!(prefix.as_ref(), b"hello");
		assert!(matches!(reader.read_chunk().unwrap(), Read::Pending));
		std::thread::scope(|scope| {
			scope.spawn(move || {
				writer.put_bytes(42, 199995);
				writer.finish().unwrap();
			});
		});
		group.finish().unwrap();
		let mut received = 5;
		while let Read::Ready(chunk) = reader.read_chunk().unwrap() {
			assert!(chunk.iter().all(|b| *b == 42));
			received += chunk.len();
		}
		assert_eq!(received, 200000);
		reader.finish().unwrap();
		drop(group);
		assert_eq!(prefix.as_ref(), b"hello");
		assert!(matches!(consumer.next().unwrap(), Read::End));
	}
}

#[test]
fn abort_and_wrong_size_are_terminal() {
	for layout in [Layout::Indexed, Layout::Packed] {
		let mut group = Group::new(layout);
		let mut consumer = group.consumer();
		let mut writer = group.create(Header { timestamp: 0, size: 4 }).unwrap();
		writer.put_slice(b"ab");
		writer.publish();
		let Read::Ready(mut reader) = consumer.next().unwrap() else {
			panic!()
		};
		let Read::Ready(held) = reader.read_chunk().unwrap() else {
			panic!()
		};
		assert_eq!(writer.finish(), Err(Error::WrongSize));
		assert_eq!(reader.read_chunk(), Err(Error::Aborted));
		assert_eq!(held.as_ref(), b"ab");
		assert!(matches!(
			group.create(Header { timestamp: 0, size: 0 }),
			Err(Error::Closed)
		));
	}
}

#[test]
fn zero_frames_cloned_readers_and_owned_slices() {
	for layout in [Layout::Indexed, Layout::Packed] {
		let mut group = Group::new(layout);
		let payload = Bytes::from(vec![17; 128]);
		let slice = payload.slice(9..109);
		let ptr = slice.as_ptr();
		group
			.create(Header { timestamp: 0, size: 0 })
			.unwrap()
			.finish()
			.unwrap();
		let mut frame = group
			.create(Header {
				timestamp: 1,
				size: 100,
			})
			.unwrap();
		frame.write_owned(slice).unwrap();
		frame.finish().unwrap();
		group.finish().unwrap();
		let consumer = group.consumer();
		for mut consumer in [consumer.clone(), consumer] {
			let Read::Ready(frame) = consumer.next().unwrap() else {
				panic!()
			};
			frame.finish().unwrap();
			let Read::Ready(mut frame) = consumer.next().unwrap() else {
				panic!()
			};
			let Read::Ready(chunk) = frame.read_chunk().unwrap() else {
				panic!()
			};
			assert_eq!(chunk.as_ptr(), ptr);
			frame.finish().unwrap();
			assert!(matches!(consumer.next().unwrap(), Read::End));
		}
	}
}

#[test]
fn bufmut_bounds_and_dropped_writer() {
	let mut group = Group::new(Layout::Indexed);
	let mut consumer = group.consumer();
	{
		let mut writer = group.create(Header { timestamp: 0, size: 1 }).unwrap();
		assert_eq!(writer.chunk_mut().len(), 1);
		writer.put_u8(42);
		assert_eq!(writer.remaining_mut(), 0);
		assert_eq!(writer.chunk_mut().len(), 0);
	}
	assert!(matches!(consumer.next(), Err(Error::Aborted)));
	assert!(matches!(
		Group::new(Layout::Indexed).create(Header {
			timestamp: 0,
			size: u64::MAX
		}),
		Err(Error::TooLarge)
	));
}

#[test]
fn mixed_direct_and_owned_writes_preserve_order() {
	for layout in [Layout::Indexed, Layout::Packed] {
		let mut group = Group::new(layout);
		let mut writer = group.create(Header { timestamp: 0, size: 6 }).unwrap();
		writer.put_slice(b"ab");
		writer.write_owned(Bytes::from_static(b"cd")).unwrap();
		writer.put_slice(b"ef");
		writer.finish().unwrap();
		group.finish().unwrap();
		let mut consumer = group.consumer();
		let Read::Ready(mut reader) = consumer.next().unwrap() else {
			panic!()
		};
		let mut result = Vec::new();
		while let Read::Ready(bytes) = reader.read_chunk().unwrap() {
			result.extend_from_slice(&bytes);
		}
		assert_eq!(result, b"abcdef");
		reader.finish().unwrap();
	}
}

#[test]
fn abandoning_a_reader_refuses_further_reads() {
	let mut group = Group::new(Layout::Indexed);
	let mut writer = group.create(Header { timestamp: 0, size: 4 }).unwrap();
	writer.put_slice(b"data");
	writer.finish().unwrap();
	group.finish().unwrap();
	let mut consumer = group.consumer();
	let Read::Ready(reader) = consumer.next().unwrap() else {
		panic!()
	};
	drop(reader);
	assert!(matches!(consumer.next(), Err(Error::Closed)));
}
