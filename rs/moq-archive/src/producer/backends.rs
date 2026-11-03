//! The same recording on every backend, including the S3 HTTP adapter rather than a listing fake.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Uri};
use bytes::Bytes;
use futures::TryStreamExt;
use moq_net::{Timescale, Timestamp, broadcast, group, track};
use object_store::aws::{AmazonS3, AmazonS3Builder, S3ConditionalPut};
use object_store::local::LocalFileSystem;
use object_store::memory::InMemory;
use object_store::path::Path;
use s3s::service::S3ServiceBuilder;

use super::*;

const VIDEO: &str = "video/1080p";
const CHAT: &str = "chat.json";

/// Real time, so the S3 client's own timers behave: a flush short enough to store the untimed
/// group while the test waits, and long enough that no put outlasts it.
fn config() -> Config {
	Config::default()
		.with_duration(Duration::from_secs(2))
		.with_flush(Duration::from_millis(200))
		.with_max_delay(Duration::from_secs(10))
}

/// A source with `groups` one-second video groups, and the chat's groups as `(sequence, frames)`.
fn source(groups: u64, chat: &[(u64, &[&str])]) -> (broadcast::Producer, [track::Producer; 2]) {
	let source = broadcast::Info::new().produce();
	let video = source
		.create_track(VIDEO, track::Info::default().with_timescale(Timescale::MILLI))
		.unwrap();
	for sequence in 0..groups {
		let mut group = video.create_group(group::Info { sequence }).unwrap();
		for frame in 0..2 {
			let pts = Timestamp::from_millis(sequence * 1_000 + frame * 500).unwrap();
			group.write_frame(pts, format!("v{sequence}.{frame}")).unwrap();
		}
		group.finish().unwrap();
	}
	let text = source.create_track(CHAT, track::Info::default()).unwrap();
	for (sequence, frames) in chat {
		let mut group = text.create_group(group::Info { sequence: *sequence }).unwrap();
		for frame in *frames {
			group.write_frame(None, frame.to_string()).unwrap();
		}
		group.finish().unwrap();
	}
	(source, [video, text])
}

fn path<S: ObjectStore>(store: &Store<S>, track: &str, name: &str) -> Path {
	Path::parse(format!(
		"{}/{}/{name}",
		store.prefix(),
		crate::path::encode_track(track).unwrap()
	))
	.unwrap()
}

/// Wait until every one of `paths` is stored.
async fn stored<S: ObjectStore>(store: &Store<S>, paths: &[Path]) {
	kio::clock::timeout(Duration::from_secs(10), async {
		for path in paths {
			while store.inner().head(path).await.is_err() {
				kio::clock::sleep(Duration::from_millis(10)).await;
			}
		}
	})
	.await
	.expect("the producer stores the objects");
}

/// Record in two runs: the first is stopped once it stored a split timed track and the head of
/// an untimed group, and the second resumes both, continuing that group in a new object.
async fn record<S: ObjectStore + Clone>(store: &Store<S>) {
	let (first, _tracks) = source(4, &[(0, &["c0"])]);
	let run = tokio::spawn(Producer::new(store.clone(), first.consume(), config()).run());
	stored(
		store,
		&[
			path(
				store,
				VIDEO,
				"0000000000000000000-0000000000000000000-0000000000000000000",
			),
			path(
				store,
				VIDEO,
				"0000000000000000002-0000000000000000000-0000000000000002000",
			),
			path(store, CHAT, "0000000000000000000-0000000000000000000"),
		],
	)
	.await;
	run.abort();
	assert!(run.await.unwrap_err().is_cancelled());

	let (second, tracks) = source(6, &[(0, &["c0", "c1"]), (1, &["c2"])]);
	let run = tokio::spawn(Producer::new(store.clone(), second.consume(), config()).run());
	for track in &tracks {
		while track.subscription().is_none() {
			kio::clock::sleep(Duration::from_millis(1)).await;
		}
		track.finish().unwrap();
	}
	second.close();
	kio::clock::timeout(Duration::from_secs(10), run)
		.await
		.expect("the producer ends with its broadcast")
		.unwrap()
		.unwrap();
}

/// Every object under the store's prefix, by path relative to it.
async fn objects<S: ObjectStore>(store: &Store<S>) -> BTreeMap<String, Bytes> {
	let metas: Vec<_> = store.inner().list(Some(store.prefix())).try_collect().await.unwrap();
	let mut objects = BTreeMap::new();
	for meta in metas {
		let bytes = store.inner().get(&meta.location).await.unwrap().bytes().await.unwrap();
		let key = meta.location.as_ref().strip_prefix("rec/").unwrap().to_string();
		objects.insert(key, bytes);
	}
	objects
}

fn decode(objects: &BTreeMap<String, Bytes>, key: &str, timed: bool) -> Vec<(u64, Vec<String>)> {
	let object = crate::layout::Object::decode(objects[key].clone(), timed).unwrap();
	object
		.entries
		.iter()
		.map(|entry| match entry {
			crate::layout::Entry::Group { sequence, frames } => (
				*sequence,
				frames
					.iter()
					.map(|frame| String::from_utf8(frame.payload.to_vec()).unwrap())
					.collect(),
			),
			crate::layout::Entry::Datagram { .. } => panic!("no datagrams were sent"),
		})
		.collect()
}

#[tokio::test]
async fn recordings_are_byte_identical_on_every_backend() {
	let memory = Store::new(InMemory::new(), "rec");
	record(&memory).await;
	let expected = objects(&memory).await;
	assert_eq!(
		expected.keys().map(String::as_str).collect::<Vec<_>>(),
		[
			".end",
			"chat%2Ejson/.info",
			"chat%2Ejson/0000000000000000000-0000000000000000000",
			"chat%2Ejson/0000000000000000000-0000000000000000001",
			"chat%2Ejson/end",
			"video%2F1080p/.info",
			"video%2F1080p/0000000000000000000-0000000000000000000-0000000000000000000",
			"video%2F1080p/0000000000000000002-0000000000000000000-0000000000000002000",
			"video%2F1080p/0000000000000000004-0000000000000000000-0000000000000004000",
			"video%2F1080p/end",
		]
	);
	assert_eq!(
		decode(&expected, "chat%2Ejson/0000000000000000000-0000000000000000000", false),
		[(0, vec!["c0".into()])],
	);
	assert_eq!(
		decode(&expected, "chat%2Ejson/0000000000000000000-0000000000000000001", false),
		[(0, vec!["c1".into()]), (1, vec!["c2".into()])],
		"the resumed group continues in its own object"
	);
	assert_eq!(
		decode(
			&expected,
			"video%2F1080p/0000000000000000004-0000000000000000000-0000000000000004000",
			true
		),
		[
			(4, vec!["v4.0".into(), "v4.1".into()]),
			(5, vec!["v5.0".into(), "v5.1".into()])
		],
		"the resumed timed track continues after its last stored group"
	);

	let dir = tempfile::tempdir().unwrap();
	let local = Store::new(LocalFileSystem::new_with_prefix(dir.path()).unwrap(), "rec");
	record(&local).await;
	assert_eq!(objects(&local).await, expected, "local disk");

	let server = Server::new().await;
	record(&server.store).await;
	assert_eq!(objects(&server.store).await, expected, "S3");
	let requests = server.requests.lock().unwrap();
	assert!(
		requests
			.iter()
			.any(|request| request.method == Method::PUT && request.conditional),
		"timed objects are created conditionally"
	);
	assert!(
		requests
			.iter()
			.any(|request| request.uri.path().contains("chat%252Ejson")),
		"a percent-encoded key survives URL encoding"
	);
}

struct Request {
	method: Method,
	uri: Uri,
	conditional: bool,
}

/// An S3 endpoint over a temporary directory, recording each request it serves.
struct Server {
	store: Store<AmazonS3>,
	requests: Arc<Mutex<Vec<Request>>>,
	task: tokio::task::JoinHandle<()>,
	_dir: tempfile::TempDir,
}

impl Server {
	async fn new() -> Self {
		let dir = tempfile::tempdir().unwrap();
		std::fs::create_dir(dir.path().join("archive")).unwrap();
		let service = S3ServiceBuilder::new(s3s_fs::FileSystem::new(dir.path()).unwrap()).build();
		let requests = Arc::new(Mutex::new(Vec::new()));
		let seen = requests.clone();
		let app = axum::Router::new().fallback(move |request: axum::extract::Request| {
			let service = service.clone();
			let seen = seen.clone();
			async move {
				seen.lock().unwrap().push(Request {
					method: request.method().clone(),
					uri: request.uri().clone(),
					conditional: request.headers().get("if-none-match").is_some_and(|value| value == "*"),
				});
				service
					.call(request.map(s3s::Body::http_body_unsync))
					.await
					.unwrap()
					.map(Body::new)
			}
		});
		let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
			.await
			.unwrap();
		let endpoint = format!("http://{}", listener.local_addr().unwrap());
		let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
		let store = AmazonS3Builder::new()
			.with_bucket_name("archive")
			.with_region("us-east-1")
			.with_endpoint(endpoint)
			.with_allow_http(true)
			.with_skip_signature(true)
			.with_conditional_put(S3ConditionalPut::ETagMatch)
			.build()
			.unwrap();
		Self {
			store: Store::new(store, "rec"),
			requests,
			task,
			_dir: dir,
		}
	}
}

impl Drop for Server {
	fn drop(&mut self) {
		self.task.abort();
	}
}
