//! KDE StatusNotifierItem and session-local transcript history.
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{MenuItem, StandardItem};
use std::collections::VecDeque;
use std::sync::mpsc;
use std::thread;

use crate::daemon::DaemonOutput;
use crate::output::{self, OutputConfig, OutputMode};

const HISTORY_LIMIT: usize = 10;

pub struct TranscriptTray {
    listening: bool,
    transcripts: VecDeque<String>,
    copy_to_clipboard: Box<dyn Fn(&str) + Send>,
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
    pub fn with_clipboard(copy: impl Fn(&str) + Send + 'static) -> Self {
        Self {
            listening: false,
            transcripts: VecDeque::new(),
            copy_to_clipboard: Box::new(copy),
        }
    }

    pub fn set_listening(&mut self, listening: bool) {
        self.listening = listening;
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
        if self.listening {
            "SoundVibes: Listening"
        } else {
            "SoundVibes: Not listening"
        }
    }
}

impl ksni::Tray for TranscriptTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "soundvibes".into()
    }

    fn title(&self) -> String {
        self.label().into()
    }

    fn icon_name(&self) -> String {
        if self.listening {
            "audio-input-microphone"
        } else {
            "microphone-sensitivity-muted"
        }
        .into()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            icon_name: self.icon_name(),
            title: self.label().into(),
            description: "Click to see recent transcripts; click a transcript to copy it".into(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let label = |text: &str| {
            StandardItem {
                // D-Bus menus interpret underscores as mnemonic markers.
                label: text.replace('_', "__"),
                enabled: false,
                ..Default::default()
            }
            .into()
        };
        let mut items = vec![label(self.label()), MenuItem::Separator];
        if self.transcripts.is_empty() {
            items.push(label("No transcripts yet"));
        } else {
            items.extend(self.recent_transcripts().map(|text| {
                let text = text.to_string();
                StandardItem {
                    label: text.replace('_', "__"),
                    activate: Box::new(move |tray: &mut Self| (tray.copy_to_clipboard)(&text)),
                    ..Default::default()
                }
                .into()
            }));
        }
        items
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

    fn listening_changed(&mut self, listening: bool) {
        self.output.listening_changed(listening);
        if let Some(tray) = &self.tray {
            tray.update(|tray| tray.set_listening(listening));
        }
    }

    fn transcript(&mut self, text: &str) {
        self.output.transcript(text);
        if let Some(tray) = &self.tray {
            tray.update(|tray| tray.push_transcript(text));
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
