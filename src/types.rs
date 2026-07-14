use clap::ValueEnum;
use serde::Deserialize;

#[derive(Debug, Copy, Clone, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Plain,
    Jsonl,
}

#[derive(Debug, Copy, Clone, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioHost {
    Default,
    Alsa,
}

impl AudioHost {
    pub fn default_for_platform() -> Self {
        AudioHost::Alsa
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VadMode {
    On,
    Off,
    Continuous,
}
