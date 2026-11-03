#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandwidthUsage {
    Normal,
    Underusing,
    Overusing,
}
