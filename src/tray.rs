//! KDE StatusNotifierItem and session-local transcript history.
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{MenuItem, StandardItem};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use crate::daemon::{Activity, DaemonOutput};
use crate::output::{self, OutputConfig, OutputMode};
use crate::popup::{self, PopupContent};

const HISTORY_LIMIT: usize = 10;

pub struct TranscriptTray {
    activity: Activity,
    transcripts: VecDeque<String>,
    recording_text: String,
    copy_to_clipboard: Arc<dyn Fn(&str) + Send + Sync>,
    popup_open: Arc<AtomicBool>,
}

impl Default for TranscriptTray {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel::<String>();
        // Serialize clipboard writes away from the D-Bus menu and audio threads.
        if let Err(err) = thread::Builder::new()
            .name("tray-clipboard".into())
            .spawn(move || {
                let config = OutputConfig {
                    mode: OutputMode::Clipboard,
                    ..OutputConfig::default()
                };
                for text in receiver {
                    if let Err(err) = output::output_text(&text, &config) {
                        eprintln!("warn: could not copy transcript: {err}");
                    }
                }
            })
        {
            eprintln!("warn: could not start tray clipboard worker: {err}");
        }
        Self::with_clipboard(move |text| {
            if let Err(err) = sender.send(text.to_string()) {
                eprintln!("warn: tray clipboard worker unavailable: {err}");
            }
        })
    }
}

impl TranscriptTray {
    /// Supply a clipboard dispatcher. It must return promptly to keep the menu responsive.
    pub fn with_clipboard(copy: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self {
            activity: Activity::Idle,
            transcripts: VecDeque::new(),
            recording_text: String::new(),
            copy_to_clipboard: Arc::new(copy),
            popup_open: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_activity(&mut self, activity: Activity) {
        self.activity = activity;
    }

    /// Accumulate decoded segments while the recording is in progress or finishing.
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

    /// Publish only after every pending segment from this recording has arrived.
    pub fn finish_recording(&mut self) {
        let text = std::mem::take(&mut self.recording_text);
        self.push_transcript(&text);
    }

    pub fn push_transcript(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.transcripts.push_front(text.to_string());
        self.transcripts.truncate(HISTORY_LIMIT);
    }

    /// Completed transcripts, newest first. Text is retained verbatim.
    pub fn recent_transcripts(&self) -> impl Iterator<Item = &str> {
        self.transcripts.iter().map(String::as_str)
    }

    fn label(&self) -> &'static str {
        match self.activity {
            Activity::Idle => "SoundVibes: Idle",
            Activity::Listening => "SoundVibes: Listening",
            Activity::Processing => "SoundVibes: Processing",
        }
    }
}

/// A click uses the same immutable recording snapshot displayed by the popup.
pub struct HistorySnapshot {
    pub content: PopupContent,
    copy_to_clipboard: Arc<dyn Fn(&str) + Send + Sync>,
}

impl HistorySnapshot {
    pub fn copy_selected(&self, index: usize) -> bool {
        if let Some(text) = self.content.transcripts.get(index) {
            (self.copy_to_clipboard)(text);
            true
        } else {
            false
        }
    }
}

impl TranscriptTray {
    pub fn history_popup(&self, x: i32, y: i32) -> HistorySnapshot {
        HistorySnapshot {
            content: PopupContent {
                title: self.label().to_string(),
                transcripts: self.recent_transcripts().map(str::to_string).collect(),
                x,
                y,
            },
            copy_to_clipboard: Arc::clone(&self.copy_to_clipboard),
        }
    }
}

impl ksni::Tray for TranscriptTray {
    fn id(&self) -> String {
        "soundvibes".into()
    }

    fn title(&self) -> String {
        self.label().into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let rgb = match self.activity {
            Activity::Idle => [145, 145, 145],
            Activity::Listening => [235, 45, 50],
            Activity::Processing => [245, 170, 35],
        };
        [16, 22, 32, 48]
            .into_iter()
            .map(|size| recording_dot(size, rgb))
            .collect()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            icon_pixmap: self.icon_pixmap(),
            title: self.label().into(),
            description: "Click to see recent transcripts; click a transcript to copy it".into(),
            ..Default::default()
        }
    }

    fn activate(&mut self, x: i32, y: i32) {
        if self.popup_open.swap(true, Ordering::AcqRel) {
            return;
        }
        let history = self.history_popup(x, y);
        let open = Arc::clone(&self.popup_open);
        let token = ksni::current_xdg_activation_token();
        if let Err(err) = thread::Builder::new()
            .name("history-popup".into())
            .spawn(move || {
                match popup::show(&history.content, token) {
                    Ok(Some(index)) => {
                        history.copy_selected(index);
                    }
                    Ok(None) => {}
                    Err(err) => eprintln!("warn: {err}"),
                }
                open.store(false, Ordering::Release);
            })
        {
            self.popup_open.store(false, Ordering::Release);
            eprintln!("warn: failed to start history popup: {err}");
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![StandardItem {
            label: "Open recording history".into(),
            activate: Box::new(|tray: &mut Self| {
                tray.activate(0, 0);
            }),
            ..Default::default()
        }
        .into()]
    }
}

/// Adds desktop feedback without changing the configured transcript destination.
pub(crate) struct DesktopOutput<'a> {
    output: &'a mut dyn DaemonOutput,
    tray: Option<Handle<TranscriptTray>>,
}

impl<'a> DesktopOutput<'a> {
    pub(crate) fn new(output: &'a mut dyn DaemonOutput) -> Self {
        // Wait for the watcher when the user service starts before Plasma does.
        let tray = match TranscriptTray::default().assume_sni_available(true).spawn() {
            Ok(tray) => Some(tray),
            Err(err) => {
                output.stderr(&format!("warn: system tray unavailable: {err}"));
                None
            }
        };
        Self { output, tray }
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
        if let Some(tray) = &self.tray {
            tray.update(|tray| tray.set_activity(activity));
        }
    }

    fn transcript(&mut self, text: &str) {
        self.output.transcript(text);
        if let Some(tray) = &self.tray {
            tray.update(|tray| tray.append_segment(text));
        }
    }

    fn recording_finished(&mut self) {
        self.output.recording_finished();
        if let Some(tray) = &self.tray {
            tray.update(|tray| tray.finish_recording());
        }
    }
}

impl Drop for DesktopOutput<'_> {
    fn drop(&mut self) {
        if let Some(tray) = &self.tray {
            tray.shutdown();
        }
    }
}

/// Network-order ARGB pixels, with a one-pixel antialiased circle edge.
fn recording_dot(size: i32, rgb: [u8; 3]) -> ksni::Icon {
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    let center = size as f32 / 2.0;
    let radius = size as f32 * 0.35;
    for y in 0..size {
        for x in 0..size {
            let distance =
                ((x as f32 + 0.5 - center).powi(2) + (y as f32 + 0.5 - center).powi(2)).sqrt();
            let alpha = ((radius + 0.5 - distance).clamp(0.0, 1.0) * 255.0).round() as u8;
            data.extend_from_slice(&[alpha, rgb[0], rgb[1], rgb[2]]);
        }
    }
    ksni::Icon {
        width: size,
        height: size,
        data,
    }
}
