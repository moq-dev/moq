// cargo run --example basic

use std::time::{Duration, SystemTime};

fn main() -> anyhow::Result<()> {
	// Generate an HMAC key with a random key ID.
	let key = moq_token::Key::generate(moq_token::Algorithm::HS256, Some(moq_token::KeyId::random()))?;

	// Serialize the key to a JWK JSON string.
	let key_str = key.to_str()?;
	println!("Generated key:\n{key_str}\n");

	// Create claims for the token.
	let claims = moq_token::Claims {
		root: "demo".to_string(),
		publish: vec!["my-stream".to_string()], // Can publish to demo/my-stream
		subscribe: vec!["".to_string()],        // Can subscribe to anything under demo/
		cluster: false,
		expires: Some(SystemTime::now() + Duration::from_secs(3600)),
		issued: Some(SystemTime::now()),
	};

	// Validate the claims (ensures at least one publish or subscribe path).
	claims.validate()?;

	// Sign a JWT token.
	let token = key.encode(&claims)?;
	println!("Signed token:\n{token}\n");

	// Verify the token.
	let verified = key.decode(&token)?;
	println!("Verified claims:");
	println!("  root: {}", verified.root);
	println!("  publish: {:?}", verified.publish);
	println!("  subscribe: {:?}", verified.subscribe);

	// Load the key back from its serialized form.
	let loaded = moq_token::Key::from_str(&key_str)?;
	let also_verified = loaded.decode(&token)?;
	assert_eq!(also_verified.root, verified.root);
	println!("\nKey round-trip successful!");

	Ok(())
}
