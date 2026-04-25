// cargo run --example asymmetric
//
// Demonstrates asymmetric key usage where the private key signs tokens
// and the public key verifies them. This is the recommended approach for
// production: the relay only needs the public key.

use std::time::{Duration, SystemTime};

fn main() -> anyhow::Result<()> {
	// Generate an ECDSA P-256 key pair.
	let private_key = moq_token::Key::generate(moq_token::Algorithm::ES256, Some(moq_token::KeyId::random()))?;
	println!("Private key:\n{}\n", private_key.to_str()?);

	// Extract the public key for the relay.
	let public_key = private_key.to_public()?;
	println!("Public key (give this to the relay):\n{}\n", public_key.to_str()?);

	// Sign a token with the private key.
	let claims = moq_token::Claims {
		root: "rooms/meeting-123".to_string(),
		publish: vec!["alice".to_string()],
		subscribe: vec!["".to_string()],
		expires: Some(SystemTime::now() + Duration::from_secs(3600)),
		issued: Some(SystemTime::now()),
	};

	let token = private_key.encode(&claims)?;
	println!("Signed token:\n{token}\n");

	// Verify with the public key (this is what the relay does).
	let verified = public_key.decode(&token)?;
	println!("Verified with public key:");
	println!("  root: {}", verified.root);
	println!("  publish: {:?}", verified.publish);
	println!("  subscribe: {:?}", verified.subscribe);

	Ok(())
}
