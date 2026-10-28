//! Subscriber age budgets from the MoQ Timestamp extension.

use super::{ParameterVarInt, Parameters, Version};

pub(super) const OPTION: u64 = 0x915C4;
pub(super) const PARAMETER: u64 = 0x915C6;
pub(super) const MAX_MILLIS: u64 = (1_u64 << 53) - 1;

pub(crate) fn from_setup(params: &Parameters, _version: Version) -> bool {
	params.get_varint(ParameterVarInt::MaxAge).is_some()
}

pub(crate) fn into_setup(params: &mut Parameters, _version: Version) {
	params.set_varint(ParameterVarInt::MaxAge, 1);
}

pub(super) fn validate(value: Option<u64>) -> Result<Option<u64>, crate::DecodeError> {
	if value.is_some_and(|value| value > MAX_MILLIS) {
		return Err(crate::DecodeError::InvalidValue);
	}
	Ok(value)
}

pub(super) fn millis(delay: std::time::Duration) -> u64 {
	delay.as_millis().min(MAX_MILLIS as u128) as u64
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn negotiated_separately_and_bounded() {
		let version = Version::Draft22;
		let mut params = Parameters::default();
		assert!(!from_setup(&params, version));
		into_setup(&mut params, version);
		assert!(from_setup(&params, version));
		assert_eq!(validate(None).unwrap(), None);
		assert_eq!(validate(Some(0)).unwrap(), Some(0));
		assert!(validate(Some(MAX_MILLIS + 1)).is_err());
		assert_eq!(millis(std::time::Duration::MAX), MAX_MILLIS);
	}
}
