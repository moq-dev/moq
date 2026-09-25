use std::fmt;

/// Memory used for buffer exchange
#[allow(clippy::unreadable_literal)]
#[rustfmt::skip]
#[repr(u32)]
pub enum Memory {
    Mmap        = 1,
    UserPtr     = 2,
    Overlay     = 3,
    DmaBuf      = 4,
}

impl fmt::Display for Memory {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match *self {
			Memory::Mmap => write!(f, "memory-mapped"),
			Memory::UserPtr => write!(f, "user pointer"),
			Memory::Overlay => write!(f, "overlay"),
			Memory::DmaBuf => write!(f, "DMA buffered"),
		}
	}
}
