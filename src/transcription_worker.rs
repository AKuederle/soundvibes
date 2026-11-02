use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use crate::error::AppError;
use crate::vocabulary::Vocabulary;

pub trait Transcriber: Send {
    /// Set the next decode's vocabulary, returning a capacity warning if needed.
    /// On error, retain the previous setting so dictation can continue.
    fn set_vocabulary(&mut self, words: &[String]) -> Result<Option<String>, AppError>;
    fn transcribe(&self, samples: &[f32], language: Option<&str>) -> Result<String, AppError>;
    /// Return false to use offline recognition for this recording.
    fn begin_stream(&mut self, _language: Option<&str>) -> Result<bool, AppError> {
        Ok(false)
    }
    fn feed_stream(&mut self, _samples: &[f32]) -> Result<(), AppError> {
        Ok(())
    }
    fn finish_stream(&mut self) -> Result<String, AppError> {
        Err(AppError::runtime("streaming unavailable"))
    }
    fn cancel_stream(&mut self) {}
}

pub struct TranscriptionJob {
    pub samples: Vec<f32>,
    pub duration_ms: u64,
    pub language: Option<String>,
    pub had_overlap: bool,
}

pub struct TranscriptionResult {
    pub index: u64,
    pub duration_ms: u64,
    pub transcript: Result<String, AppError>,
    pub had_overlap: bool,
    pub warnings: Vec<String>,
}

enum WorkerCommand {
    Begin(Option<String>),
    Feed(Vec<f32>),
    Cancel,
    Transcribe { index: u64, job: TranscriptionJob },
    Shutdown,
}

pub struct TranscriptionWorker {
    jobs: Sender<WorkerCommand>,
    results: Receiver<TranscriptionResult>,
    handle: Option<JoinHandle<()>>,
    next_index: u64,
    pending: usize,
}

impl TranscriptionWorker {
    pub fn start(mut transcriber: Box<dyn Transcriber>, vocabulary_dir: Option<PathBuf>) -> Self {
        let (job_sender, job_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();

        let handle = thread::spawn(move || {
            let mut vocabulary = Vocabulary::new(vocabulary_dir);
            let mut streaming = false;
            let mut stream_warnings = Vec::new();
            while let Ok(command) = job_receiver.recv() {
                match command {
                    WorkerCommand::Begin(language) => {
                        transcriber.cancel_stream();
                        stream_warnings = refresh_vocabulary(&mut vocabulary, transcriber.as_mut());
                        streaming = match transcriber.begin_stream(language.as_deref()) {
                            Ok(active) => active,
                            Err(error) => {
                                stream_warnings.push(format!(
                                    "stream begin: {error}; using offline recognition"
                                ));
                                false
                            }
                        };
                    }
                    WorkerCommand::Feed(samples) => {
                        if streaming {
                            if let Err(error) = transcriber.feed_stream(&samples) {
                                stream_warnings.push(format!(
                                    "stream feed: {error}; using offline recognition"
                                ));
                                transcriber.cancel_stream();
                                streaming = false;
                            }
                        }
                    }
                    WorkerCommand::Cancel => {
                        transcriber.cancel_stream();
                        streaming = false;
                        stream_warnings.clear();
                    }

                    WorkerCommand::Transcribe { index, job } => {
                        let mut warnings = std::mem::take(&mut stream_warnings);
                        let transcript = if streaming {
                            streaming = false;
                            match transcriber.finish_stream() {
                                Ok(text) => Ok(text),
                                Err(error) => {
                                    warnings.push(format!(
                                        "stream finish: {error}; using offline recognition"
                                    ));
                                    transcriber.cancel_stream();
                                    warnings.extend(refresh_vocabulary(
                                        &mut vocabulary,
                                        transcriber.as_mut(),
                                    ));
                                    transcriber.transcribe(&job.samples, job.language.as_deref())
                                }
                            }
                        } else {
                            warnings
                                .extend(refresh_vocabulary(&mut vocabulary, transcriber.as_mut()));
                            transcriber.transcribe(&job.samples, job.language.as_deref())
                        };
                        let result = TranscriptionResult {
                            index,
                            duration_ms: job.duration_ms,
                            transcript,
                            had_overlap: job.had_overlap,
                            warnings,
                        };
                        if result_sender.send(result).is_err() {
                            break;
                        }
                    }
                    WorkerCommand::Shutdown => {
                        transcriber.cancel_stream();
                        break;
                    }
                }
            }
        });

        Self {
            jobs: job_sender,
            results: result_receiver,
            handle: Some(handle),
            next_index: 1,
            pending: 0,
        }
    }

    pub fn begin_stream(&mut self, language: Option<String>) -> Result<(), AppError> {
        self.send(WorkerCommand::Begin(language))
    }
    pub fn feed_stream(&mut self, samples: Vec<f32>) -> Result<(), AppError> {
        self.send(WorkerCommand::Feed(samples))
    }
    pub fn cancel_stream(&mut self) -> Result<(), AppError> {
        self.send(WorkerCommand::Cancel)
    }
    fn send(&self, command: WorkerCommand) -> Result<(), AppError> {
        self.jobs
            .send(command)
            .map_err(|e| AppError::runtime(format!("transcription worker stopped: {e}")))
    }

    pub fn submit(&mut self, job: TranscriptionJob) -> Result<(), AppError> {
        let index = self.next_index;
        self.jobs
            .send(WorkerCommand::Transcribe { index, job })
            .map_err(|err| AppError::runtime(format!("transcription worker stopped: {err}")))?;
        self.next_index += 1;
        self.pending += 1;
        Ok(())
    }

    pub fn try_recv(&mut self) -> Option<TranscriptionResult> {
        let result = self.results.try_recv().ok()?;
        self.pending = self.pending.saturating_sub(1);
        Some(result)
    }

    pub fn recv(&mut self) -> Option<TranscriptionResult> {
        let result = self.results.recv().ok()?;
        self.pending = self.pending.saturating_sub(1);
        Some(result)
    }

    pub fn has_pending(&self) -> bool {
        self.pending > 0
    }

    pub fn shutdown(&mut self) -> Result<(), AppError> {
        let _ = self.jobs.send(WorkerCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_| AppError::runtime("transcription worker panicked"))?;
        }
        Ok(())
    }
}

fn refresh_vocabulary(
    vocabulary: &mut Vocabulary,
    transcriber: &mut dyn Transcriber,
) -> Vec<String> {
    let (words, warning) = vocabulary.refresh();
    let mut warnings: Vec<String> = warning.into_iter().collect();
    match transcriber.set_vocabulary(words) {
        Ok(warning) => warnings.extend(warning),
        Err(error) => warnings.push(format!(
            "vocabulary: {error}; keeping the previous vocabulary"
        )),
    }
    warnings
}

impl Drop for TranscriptionWorker {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::{Transcriber, TranscriptionJob, TranscriptionWorker};
    use crate::error::AppError;

    struct StreamTranscriber {
        seen: Vec<f32>,
        fail_feed: bool,
        fed: Option<mpsc::Sender<()>>,
    }
    impl Transcriber for StreamTranscriber {
        fn set_vocabulary(&mut self, _: &[String]) -> Result<Option<String>, AppError> {
            Ok(None)
        }
        fn transcribe(&self, samples: &[f32], _: Option<&str>) -> Result<String, AppError> {
            Ok(format!("offline:{samples:?}"))
        }
        fn begin_stream(&mut self, _: Option<&str>) -> Result<bool, AppError> {
            self.seen.clear();
            Ok(true)
        }
        fn feed_stream(&mut self, samples: &[f32]) -> Result<(), AppError> {
            if self.fail_feed {
                return Err(AppError::runtime("feed failed"));
            }
            self.seen.extend_from_slice(samples);
            if let Some(sender) = &self.fed {
                let _ = sender.send(());
            }
            Ok(())
        }
        fn finish_stream(&mut self) -> Result<String, AppError> {
            Ok(format!("stream:{:?}", self.seen))
        }
        fn cancel_stream(&mut self) {
            self.seen.clear();
        }
    }
    fn job(samples: Vec<f32>) -> TranscriptionJob {
        TranscriptionJob {
            samples,
            duration_ms: 1,
            language: Some("en".into()),
            had_overlap: false,
        }
    }
    #[test]
    fn live_stream_finishes_in_order_and_cancel_discards_audio() {
        let mut worker = TranscriptionWorker::start(
            Box::new(StreamTranscriber {
                seen: vec![],
                fail_feed: false,
                fed: None,
            }),
            None,
        );
        worker.begin_stream(Some("en".into())).unwrap();
        worker.feed_stream(vec![9.0]).unwrap();
        worker.cancel_stream().unwrap();
        worker.begin_stream(Some("en".into())).unwrap();
        worker.feed_stream(vec![1.0]).unwrap();
        worker.feed_stream(vec![2.0]).unwrap();
        worker.submit(job(vec![1.0, 2.0])).unwrap();
        worker.submit(job(vec![3.0])).unwrap();
        let first = worker.recv().unwrap();
        assert_eq!(first.index, 1);
        assert_eq!(first.transcript.unwrap(), "stream:[1.0, 2.0]");
        let second = worker.recv().unwrap();
        assert_eq!(second.index, 2);
        assert_eq!(second.transcript.unwrap(), "offline:[3.0]");
    }
    #[test]
    fn failed_live_feed_falls_back_to_complete_audio() {
        let mut worker = TranscriptionWorker::start(
            Box::new(StreamTranscriber {
                seen: vec![],
                fail_feed: true,
                fed: None,
            }),
            None,
        );
        worker.begin_stream(Some("en".into())).unwrap();
        worker.feed_stream(vec![1.0]).unwrap();
        worker.submit(job(vec![1.0, 2.0])).unwrap();
        let result = worker.recv().unwrap();
        assert_eq!(result.transcript.unwrap(), "offline:[1.0, 2.0]");
        assert!(result.warnings.iter().any(|w| w.contains("feed failed")));
    }

    #[test]
    fn live_feed_runs_before_recording_is_submitted() {
        let (sender, receiver) = mpsc::channel();
        let mut worker = TranscriptionWorker::start(
            Box::new(StreamTranscriber {
                seen: vec![],
                fail_feed: false,
                fed: Some(sender),
            }),
            None,
        );
        worker.begin_stream(Some("en".into())).unwrap();
        worker.feed_stream(vec![1.0]).unwrap();
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("inference receives audio while recording");
        assert!(!worker.has_pending());
        worker.submit(job(vec![1.0])).unwrap();
        assert_eq!(worker.recv().unwrap().transcript.unwrap(), "stream:[1.0]");
    }

    #[test]
    fn shutdown_cancels_an_unfinished_session_without_emitting_text() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        struct ActiveStream(Arc<AtomicBool>);
        impl Transcriber for ActiveStream {
            fn set_vocabulary(&mut self, _: &[String]) -> Result<Option<String>, AppError> {
                Ok(None)
            }
            fn transcribe(&self, _: &[f32], _: Option<&str>) -> Result<String, AppError> {
                panic!("cancelled audio must not decode")
            }
            fn begin_stream(&mut self, _: Option<&str>) -> Result<bool, AppError> {
                self.0.store(true, Ordering::SeqCst);
                Ok(true)
            }
            fn cancel_stream(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let active = Arc::new(AtomicBool::new(false));
        let mut worker =
            TranscriptionWorker::start(Box::new(ActiveStream(Arc::clone(&active))), None);
        worker.begin_stream(Some("en".into())).unwrap();
        worker.feed_stream(vec![1.0]).unwrap();
        worker.shutdown().unwrap();
        assert!(!active.load(Ordering::SeqCst));
        assert!(worker.try_recv().is_none());
    }

    struct UnavailableStream {
        fail_begin: bool,
        unavailable: bool,
    }
    impl Transcriber for UnavailableStream {
        fn set_vocabulary(&mut self, _: &[String]) -> Result<Option<String>, AppError> {
            Ok(None)
        }
        fn transcribe(&self, samples: &[f32], _: Option<&str>) -> Result<String, AppError> {
            Ok(format!("{samples:?}"))
        }
        fn begin_stream(&mut self, _: Option<&str>) -> Result<bool, AppError> {
            if self.fail_begin {
                Err(AppError::runtime("begin failed"))
            } else {
                Ok(!self.unavailable)
            }
        }
        fn finish_stream(&mut self) -> Result<String, AppError> {
            Err(AppError::runtime("finish failed"))
        }
    }
    #[test]
    fn unavailable_begin_and_failed_finish_use_offline_audio() {
        for (fail_begin, unavailable) in [(true, false), (false, true), (false, false)] {
            let mut worker = TranscriptionWorker::start(
                Box::new(UnavailableStream {
                    fail_begin,
                    unavailable,
                }),
                None,
            );
            worker.begin_stream(Some("en".into())).unwrap();
            worker.feed_stream(vec![1.0]).unwrap();
            worker.submit(job(vec![1.0, 2.0])).unwrap();
            assert_eq!(worker.recv().unwrap().transcript.unwrap(), "[1.0, 2.0]");
        }
    }

    struct BlockingTranscriber {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl Transcriber for BlockingTranscriber {
        fn set_vocabulary(&mut self, _words: &[String]) -> Result<Option<String>, AppError> {
            Ok(None)
        }

        fn transcribe(
            &self,
            _samples: &[f32],
            _language: Option<&str>,
        ) -> Result<String, AppError> {
            self.started.send(()).expect("started signal");
            self.release.recv().expect("release signal");
            Ok("done".to_string())
        }
    }

    #[test]
    fn submit_returns_while_transcription_runs_in_background() {
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let mut worker = TranscriptionWorker::start(
            Box::new(BlockingTranscriber {
                started: started_sender,
                release: release_receiver,
            }),
            None,
        );

        worker
            .submit(TranscriptionJob {
                samples: vec![0.2; 160],
                duration_ms: 10,
                language: Some("en".to_string()),
                had_overlap: false,
            })
            .expect("submit job");

        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker started job");
        assert!(worker.try_recv().is_none());

        release_sender.send(()).expect("release worker");
        let result = loop {
            if let Some(result) = worker.try_recv() {
                break result;
            }
            thread::sleep(Duration::from_millis(5));
        };

        assert_eq!(result.index, 1);
        assert_eq!(result.transcript.expect("transcript"), "done");
        worker.shutdown().expect("shutdown worker");
    }

    struct LanguageProbeTranscriber {
        seen_language: mpsc::Sender<Option<String>>,
    }

    impl Transcriber for LanguageProbeTranscriber {
        fn set_vocabulary(&mut self, _words: &[String]) -> Result<Option<String>, AppError> {
            Ok(None)
        }

        fn transcribe(&self, _samples: &[f32], language: Option<&str>) -> Result<String, AppError> {
            self.seen_language
                .send(language.map(str::to_string))
                .expect("language signal");
            Ok("done".to_string())
        }
    }

    #[test]
    fn worker_preserves_automatic_language_detection() {
        let (language_sender, language_receiver) = mpsc::channel();
        let mut worker = TranscriptionWorker::start(
            Box::new(LanguageProbeTranscriber {
                seen_language: language_sender,
            }),
            None,
        );

        worker
            .submit(TranscriptionJob {
                samples: vec![0.2; 160],
                duration_ms: 10,
                language: None,
                had_overlap: false,
            })
            .expect("submit job");

        assert_eq!(
            language_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("language signal"),
            None
        );
        worker.shutdown().expect("shutdown worker");
    }

    struct SampleTranscriber;

    impl Transcriber for SampleTranscriber {
        fn set_vocabulary(&mut self, _words: &[String]) -> Result<Option<String>, AppError> {
            Ok(None)
        }

        fn transcribe(&self, samples: &[f32], _language: Option<&str>) -> Result<String, AppError> {
            Ok(samples.first().copied().unwrap_or_default().to_string())
        }
    }

    #[test]
    fn worker_owns_fifo_sequence_and_pending_state() {
        let mut worker = TranscriptionWorker::start(Box::new(SampleTranscriber), None);
        for sample in [1.0, 2.0] {
            worker
                .submit(TranscriptionJob {
                    samples: vec![sample],
                    duration_ms: 1,
                    language: None,
                    had_overlap: false,
                })
                .expect("submit job");
        }

        assert!(worker.has_pending());
        let first = worker.recv().expect("first result");
        assert_eq!(first.index, 1);
        assert_eq!(first.transcript.expect("first transcript"), "1");
        assert!(worker.has_pending());

        let second = worker.recv().expect("second result");
        assert_eq!(second.index, 2);
        assert_eq!(second.transcript.expect("second transcript"), "2");
        assert!(!worker.has_pending());
    }
}
