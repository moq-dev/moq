use hang::{
    catalog::Catalog,
    container::{Frame, Timestamp},
};
use std::{env, fs};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().collect();
    match args[1].as_str() {
        "fetch-supported" => {
            let version: hang::moq_net::Version = args[2].parse()?;
            let supported = match version {
                hang::moq_net::Version::Lite(version) => version.has_track_stream(),
                hang::moq_net::Version::Ietf(_) => true,
                _ => return Err("unknown protocol family".into()),
            };
            println!("{supported}");
        }
        "encode" => {
            let catalog = Catalog::<()>::from_str(
                r#"{"video":{"renditions":{"video":{"codec":"avc3.42001e","container":{"kind":"legacy"}}}},"audio":{"renditions":{}}}"#,
            )?;
            fs::write(&args[2], catalog.to_json()?)?;
            let frame = Frame {
                timestamp: Timestamp::from_micros(1000001)?,
                payload: "compat-frame".as_bytes().to_vec().into(),
            };
            let mut bytes = bytes::BytesMut::new();
            frame.encode(&mut bytes)?;
            fs::write(&args[3], bytes)?;
        }
        "decode" => {
            let catalog = Catalog::<()>::from_str(&fs::read_to_string(&args[2])?)?;
            assert_eq!(
                catalog.video.renditions["video"].codec.to_string(),
                "avc3.42001e"
            );
            let frame = Frame::decode(bytes::Bytes::from(fs::read(&args[3])?))?;
            assert_eq!(frame.timestamp, Timestamp::from_micros(1000001)?);
            assert_eq!(frame.payload.as_ref(), b"compat-frame");
        }
        _ => return Err("expected encode or decode".into()),
    }
    Ok(())
}
