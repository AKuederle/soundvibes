//! Session D-Bus interface for the native Plasma widget.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use zbus::blocking::{connection::Builder, Connection};
use zbus::zvariant::Value;

use crate::daemon::{Activity, DaemonOutput};
use crate::error::AppError;
use crate::output::{self, OutputConfig, OutputMode};

pub const BUS_NAME: &str = "org.soundvibes.Daemon";
pub const OBJECT_PATH: &str = "/org/soundvibes/Daemon";
pub const INTERFACE: &str = "org.soundvibes.Daemon";
const HISTORY_LIMIT: usize = 10;
type Clipboard = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

#[derive(Clone, Serialize, Deserialize)]
pub struct Recording {
    pub id: String,
    pub text: String,
}

#[derive(Serialize)]
pub struct RecordingHistory {
    pub activity: Activity,
    pub recordings: VecDeque<Recording>,
    #[serde(skip)]
    recording_text: String,
    #[serde(skip)]
    instance: String,
    #[serde(skip)]
    sequence: u64,
}

impl Default for RecordingHistory {
    fn default() -> Self {
        Self {
            activity: Activity::Idle,
            recordings: VecDeque::new(),
            recording_text: String::new(),
            instance: format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
            sequence: 0,
        }
    }
}

impl RecordingHistory {
    pub fn set_activity(&mut self, activity: Activity) {
        self.activity = activity;
    }

    pub fn append_segment(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if !self.recording_text.is_empty() {
            self.recording_text.push(' ');
        }
        self.recording_text.push_str(text);
    }

    pub fn finish_recording(&mut self) {
        let text = std::mem::take(&mut self.recording_text);
        self.push_transcript(&text);
    }

    pub fn push_transcript(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.sequence += 1;
        self.recordings.push_front(Recording {
            id: format!("{}-{}", self.instance, self.sequence),
            text: text.to_string(),
        });
        self.recordings.truncate(HISTORY_LIMIT);
    }

    pub fn recent_transcripts(&self) -> impl Iterator<Item = &str> {
        self.recordings
            .iter()
            .map(|recording| recording.text.as_str())
    }
}

struct DesktopService {
    history: Arc<Mutex<RecordingHistory>>,
    copy: Clipboard,
}

#[zbus::interface(name = "org.soundvibes.Daemon")]
impl DesktopService {
    #[zbus(property)]
    fn snapshot(&self) -> zbus::fdo::Result<String> {
        let history = self
            .history
            .lock()
            .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))?;
        serde_json::to_string(&*history).map_err(|err| zbus::fdo::Error::Failed(err.to_string()))
    }

    fn copy_recording(&self, id: &str) -> zbus::fdo::Result<()> {
        let text = self
            .history
            .lock()
            .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))?
            .recordings
            .iter()
            .find(|recording| recording.id == id)
            .map(|recording| recording.text.clone())
            .ok_or_else(|| {
                zbus::fdo::Error::InvalidArgs("Recording is no longer in history".into())
            })?;
        (self.copy)(&text).map_err(zbus::fdo::Error::Failed)
    }
}

pub struct DesktopBridge {
    connection: Connection,
    history: Arc<Mutex<RecordingHistory>>,
}

impl DesktopBridge {
    /// An explicit bus address supports isolated integration tests.
    pub fn connect_at(
        address: Option<&str>,
        copy: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<Self, AppError> {
        let history = Arc::new(Mutex::new(RecordingHistory::default()));
        let service = DesktopService {
            history: Arc::clone(&history),
            copy: Arc::new(copy),
        };
        let connection = (|| -> zbus::Result<Connection> {
            let builder = match address {
                Some(address) => Builder::address(address)?,
                None => Builder::session()?,
            };
            builder
                .name(BUS_NAME)?
                .serve_at(OBJECT_PATH, service)?
                .build()
        })()
        .map_err(|err| AppError::runtime(format!("desktop D-Bus unavailable: {err}")))?;
        Ok(Self {
            connection,
            history,
        })
    }

    pub fn update(&self, change: impl FnOnce(&mut RecordingHistory)) -> Result<(), AppError> {
        let encode = |history: &RecordingHistory| {
            serde_json::to_string(history).map_err(|err| AppError::runtime(err.to_string()))
        };
        let mut history = self
            .history
            .lock()
            .map_err(|err| AppError::runtime(err.to_string()))?;
        let before = encode(&history)?;
        change(&mut history);
        let after = encode(&history)?;
        drop(history);
        if before != after {
            let changed = HashMap::from([("Snapshot", Value::from(after.as_str()))]);
            self.connection
                .emit_signal(
                    None::<&str>,
                    OBJECT_PATH,
                    "org.freedesktop.DBus.Properties",
                    "PropertiesChanged",
                    &(INTERFACE, changed, Vec::<&str>::new()),
                )
                .map_err(|err| AppError::runtime(format!("desktop update failed: {err}")))?;
        }
        Ok(())
    }
}

pub(crate) struct DesktopOutput<'a> {
    output: &'a mut dyn DaemonOutput,
    bridge: Option<DesktopBridge>,
}

impl<'a> DesktopOutput<'a> {
    pub(crate) fn new(output: &'a mut dyn DaemonOutput) -> Self {
        let bridge = match DesktopBridge::connect_at(None, |text| {
            output::output_text(
                text,
                &OutputConfig {
                    mode: OutputMode::Clipboard,
                    ..OutputConfig::default()
                },
            )
            .map_err(|err| err.to_string())
        }) {
            Ok(bridge) => Some(bridge),
            Err(err) => {
                output.stderr(&format!("warn: {err}"));
                None
            }
        };
        Self { output, bridge }
    }

    fn update(&mut self, change: impl FnOnce(&mut RecordingHistory)) {
        if let Some(bridge) = &self.bridge {
            if let Err(err) = bridge.update(change) {
                self.output.stderr(&format!("warn: {err}"));
            }
        }
    }
}

impl DaemonOutput for DesktopOutput<'_> {
    fn stdout(&mut self, message: &str) {
        self.output.stdout(message);
    }
    fn stderr(&mut self, message: &str) {
        self.output.stderr(message);
    }
    fn activity_changed(&mut self, activity: Activity) {
        self.output.activity_changed(activity);
        self.update(|history| history.set_activity(activity));
    }
    fn transcript(&mut self, text: &str) {
        self.output.transcript(text);
        self.update(|history| history.append_segment(text));
    }
    fn recording_finished(&mut self) {
        self.output.recording_finished();
        self.update(RecordingHistory::finish_recording);
    }
}
