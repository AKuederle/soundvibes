use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use crate::daemon::Transcriber;
use crate::error::AppError;

pub struct TranscriptionJob {
    pub index: u64,
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
    Transcribe(TranscriptionJob),
    Shutdown,
}

pub struct TranscriptionWorker {
    jobs: Sender<WorkerCommand>,
    results: Receiver<TranscriptionResult>,
    handle: Option<JoinHandle<()>>,
}

impl TranscriptionWorker {
    pub fn start(transcriber: Box<dyn Transcriber>) -> Self {
        let (job_sender, job_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();

        let handle = thread::spawn(move || {
            while let Ok(command) = job_receiver.recv() {
                match command {
                    WorkerCommand::Transcribe(job) => {
                        let transcript =
                            transcriber.transcribe(&job.samples, job.language.as_deref());
                        let result = TranscriptionResult {
                            index: job.index,
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
        }
    }

    pub fn submit(&self, job: TranscriptionJob) -> Result<(), AppError> {
        self.jobs
            .send(WorkerCommand::Transcribe(job))
            .map_err(|err| AppError::runtime(format!("transcription worker stopped: {err}")))
    }

    pub fn try_recv(&self) -> Option<TranscriptionResult> {
        match self.results.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    pub fn recv(&self) -> Option<TranscriptionResult> {
        self.results.recv().ok()
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

    use super::{TranscriptionJob, TranscriptionWorker};
    use crate::daemon::Transcriber;
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
                index: 7,
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

        assert_eq!(result.index, 7);
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
                index: 8,
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
}
