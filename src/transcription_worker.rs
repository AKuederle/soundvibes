use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use crate::error::AppError;

pub trait Transcriber: Send {
    fn transcribe(&self, samples: &[f32], language: Option<&str>) -> Result<String, AppError>;
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
}

enum WorkerCommand {
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
    pub fn start(transcriber: Box<dyn Transcriber>) -> Self {
        let (job_sender, job_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();

        let handle = thread::spawn(move || {
            while let Ok(command) = job_receiver.recv() {
                match command {
                    WorkerCommand::Transcribe { index, job } => {
                        let transcript =
                            transcriber.transcribe(&job.samples, job.language.as_deref());
                        let result = TranscriptionResult {
                            index,
                            duration_ms: job.duration_ms,
                            transcript,
                            had_overlap: job.had_overlap,
                        };
                        if result_sender.send(result).is_err() {
                            break;
                        }
                    }
                    WorkerCommand::Shutdown => break,
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

    pub fn reload(&mut self, transcriber: Box<dyn Transcriber>) -> Result<(), AppError> {
        if self.has_pending() {
            return Err(AppError::runtime(
                "cannot reload transcriber while transcription is pending",
            ));
        }
        let next_index = self.next_index;
        self.shutdown()?;
        *self = Self::start(transcriber);
        self.next_index = next_index;
        Ok(())
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

    struct BlockingTranscriber {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl Transcriber for BlockingTranscriber {
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
        let mut worker = TranscriptionWorker::start(Box::new(BlockingTranscriber {
            started: started_sender,
            release: release_receiver,
        }));

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
        let mut worker = TranscriptionWorker::start(Box::new(LanguageProbeTranscriber {
            seen_language: language_sender,
        }));

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
        fn transcribe(&self, samples: &[f32], _language: Option<&str>) -> Result<String, AppError> {
            Ok(samples.first().copied().unwrap_or_default().to_string())
        }
    }

    #[test]
    fn worker_owns_fifo_sequence_and_pending_state() {
        let mut worker = TranscriptionWorker::start(Box::new(SampleTranscriber));
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

    #[test]
    fn worker_preserves_sequence_when_transcriber_is_reloaded() {
        let mut worker = TranscriptionWorker::start(Box::new(SampleTranscriber));
        worker
            .submit(TranscriptionJob {
                samples: vec![1.0],
                duration_ms: 1,
                language: None,
                had_overlap: false,
            })
            .expect("submit first job");
        assert_eq!(worker.recv().expect("first result").index, 1);

        worker
            .reload(Box::new(SampleTranscriber))
            .expect("reload transcriber");
        worker
            .submit(TranscriptionJob {
                samples: vec![2.0],
                duration_ms: 1,
                language: None,
                had_overlap: false,
            })
            .expect("submit second job");

        assert_eq!(worker.recv().expect("second result").index, 2);
    }
}
