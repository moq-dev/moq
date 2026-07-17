// cargo run --example basic

use std::time::{Duration, SystemTime};

fn main() -> anyhow::Result<()> {
	// Generate an HMAC key with a random key ID.
	let key = moq_token::Key::generate(moq_token::Algorithm::HS256, Some(moq_token::KeyId::random()))?;

	// Serialize the key to a JWK JSON string.
	let key_str = key.to_str()?;
	println!("Generated key:\n{key_str}\n");

	// Create claims for the token.
	let claims = moq_token::Claims::default()
		.with_root("demo")
		.with_publish(["my-stream"]) // Can publish to demo/my-stream
		.with_subscribe([""]) // Can subscribe to anything under demo/
		.with_expires(SystemTime::now() + Duration::from_secs(3600))
		.with_issued(SystemTime::now());

	// Validate the claims (ensures at least one publish or subscribe path).
	claims.validate()?;

	// Sign a JWT token.
	let token = key.sign(&claims)?;
	println!("Signed token:\n{token}\n");

	// Verify the token.
	let verified = key.verify(&token)?;
	println!("Verified claims:");
	println!("  root: {}", verified.root);
	println!("  publish: {:?}", verified.publish);
	println!("  subscribe: {:?}", verified.subscribe);

	// Scope the claims to a connection, the way a relay does. Connecting to
	// demo/my-stream consumes the "my-stream" grant, leaving publish access to the
	// path itself.
	let permissions = verified.authorize("demo/my-stream")?;
	println!("\nPermissions at demo/my-stream:");
	println!("  publish: {:?}", permissions.publish);
	println!("  subscribe: {:?}", permissions.subscribe);

	// Load the key back from its serialized form.
	let loaded = moq_token::Key::from_str(&key_str)?;
	let also_verified = loaded.verify(&token)?;
	assert_eq!(also_verified.root, verified.root);
	println!("\nKey round-trip successful!");

	Ok(())
}
