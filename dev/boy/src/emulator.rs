use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use boytacean::gb::{AudioProvider, GameBoy, GameBoyMode};
use boytacean::pad::PadKey;

pub const WIDTH: u32 = 160;
pub const HEIGHT: u32 = 144;

/// Game Boy button inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Up,
    Down,
    Left,
    Right,
    A,
    B,
    Start,
    Select,
}

impl Button {
    const ALL: [Button; 8] = [
        Button::Up,
        Button::Down,
        Button::Left,
        Button::Right,
        Button::A,
        Button::B,
        Button::Start,
        Button::Select,
    ];

    fn to_pad_key(self) -> PadKey {
        match self {
            Button::Up => PadKey::Up,
            Button::Down => PadKey::Down,
            Button::Left => PadKey::Left,
            Button::Right => PadKey::Right,
            Button::A => PadKey::A,
            Button::B => PadKey::B,
            Button::Start => PadKey::Start,
            Button::Select => PadKey::Select,
        }
    }
}

/// Wrapper around boytacean's GameBoy emulator.
///
/// Tracks per-viewer button state. A button stays pressed on the emulator
/// as long as at least one viewer is holding it (union of all viewers).
pub struct Emulator {
    gb: GameBoy,
    rom: Vec<u8>,
    /// Per-viewer held buttons.
    viewers: HashMap<String, HashSet<Button>>,
}

impl Emulator {
    /// Create a new emulator and load a ROM.
    pub fn new(rom_path: &Path) -> Result<Self> {
        let rom = std::fs::read(rom_path)
            .with_context(|| format!("failed to read ROM: {}", rom_path.display()))?;

        let mut gb = GameBoy::new(Some(GameBoyMode::Cgb));
        gb.load(false)
            .map_err(|e| anyhow::anyhow!("failed to initialize emulator: {e}"))?;
        gb.load_rom(&rom, None)
            .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;
        gb.load_boot_state();
        gb.cpu().a = 0x11;

        Ok(Self {
            gb,
            rom,
            viewers: HashMap::new(),
        })
    }

    /// Reset the emulator, reloading the ROM from scratch.
    pub fn reset(&mut self) -> Result<()> {
        self.gb.reset();
        self.gb
            .load(false)
            .map_err(|e| anyhow::anyhow!("failed to initialize emulator: {e}"))?;
        self.gb
            .load_rom(&self.rom, None)
            .map_err(|e| anyhow::anyhow!("failed to reload ROM: {e}"))?;
        self.gb.load_boot_state();
        self.gb.cpu().a = 0x11;
        self.viewers.clear();
        Ok(())
    }

    /// Advance the emulator by one frame.
    pub fn tick(&mut self) {
        self.gb.next_frame();
    }

    /// Get the framebuffer as RGBA pixels (160x144).
    pub fn framebuffer(&mut self) -> Vec<u8> {
        let rgb = self.gb.frame_buffer();
        let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);

        for pixel in rgb.chunks_exact(3) {
            rgba.push(pixel[0]); // R
            rgba.push(pixel[1]); // G
            rgba.push(pixel[2]); // B
            rgba.push(255); // A
        }

        rgba
    }

    /// Drain accumulated audio samples from the APU.
    pub fn audio_samples(&mut self) -> Vec<u8> {
        let samples: Vec<u8> = self.gb.audio_buffer().iter().copied().collect();
        self.gb.clear_audio_buffer();
        samples
    }

    /// Get the union of all currently pressed buttons across all viewers.
    pub fn pressed_buttons(&self) -> HashSet<Button> {
        self.viewers.values().flatten().copied().collect()
    }

    /// Set the full button state for a viewer, replacing any previous state.
    pub fn set_buttons(&mut self, viewer_id: &str, buttons: HashSet<Button>) {
        self.viewers.insert(viewer_id.to_string(), buttons);
        self.sync_buttons();
    }

    /// A viewer disconnected — remove their button state.
    pub fn viewer_left(&mut self, viewer_id: &str) {
        self.viewers.remove(viewer_id);
        self.sync_buttons();
    }

    /// Recompute the union of all viewer buttons and sync with the emulator.
    fn sync_buttons(&mut self) {
        let union = self.pressed_buttons();
        for button in Button::ALL {
            if union.contains(&button) {
                self.gb.key_press(button.to_pad_key());
            } else {
                self.gb.key_lift(button.to_pad_key());
            }
        }
    }
}
