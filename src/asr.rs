use std::sync::mpsc::{Receiver, Sender};
use std::thread::{self, JoinHandle};

use sherpa_rs::transducer::{TransducerConfig, TransducerRecognizer};

use crate::constants::SAMPLE_RATE;
use crate::model::ModelPaths;

#[derive(Debug)]
pub enum AsrCommand {
    Load(ModelPaths),
    Transcribe(Vec<f32>),
    Shutdown,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AsrEvent {
    Loaded(Result<(), String>),
    Recognized(Result<String, String>),
}

pub fn spawn_asr_worker(
    commands: Receiver<AsrCommand>,
    events: Sender<AsrEvent>,
) -> JoinHandle<()> {
    thread::spawn(move || run_asr_worker(commands, events, SherpaRecognizerBackend::default()))
}

trait RecognizerBackend {
    fn load(&mut self, paths: ModelPaths) -> Result<(), String>;
    fn transcribe(&mut self, samples: &[f32]) -> Result<String, String>;
}

#[derive(Default)]
struct SherpaRecognizerBackend {
    recognizer: Option<TransducerRecognizer>,
}

impl RecognizerBackend for SherpaRecognizerBackend {
    fn load(&mut self, paths: ModelPaths) -> Result<(), String> {
        self.recognizer = None;
        let config = TransducerConfig {
            encoder: paths.encoder().to_string_lossy().into_owned(),
            decoder: paths.decoder().to_string_lossy().into_owned(),
            joiner: paths.joiner().to_string_lossy().into_owned(),
            tokens: paths.tokens().to_string_lossy().into_owned(),
            model_type: "nemo_transducer".into(),
            num_threads: 2,
            sample_rate: 16_000,
            feature_dim: 80,
            decoding_method: "greedy_search".into(),
            debug: false,
            provider: None,
            ..Default::default()
        };
        self.recognizer =
            Some(TransducerRecognizer::new(config).map_err(|error| error.to_string())?);
        Ok(())
    }

    fn transcribe(&mut self, samples: &[f32]) -> Result<String, String> {
        let recognizer = self
            .recognizer
            .as_mut()
            .ok_or_else(|| "model is not loaded".to_owned())?;
        Ok(recognizer.transcribe(SAMPLE_RATE, samples))
    }
}

fn run_asr_worker<B: RecognizerBackend>(
    commands: Receiver<AsrCommand>,
    events: Sender<AsrEvent>,
    mut backend: B,
) {
    let mut model_loaded = false;

    while let Ok(command) = commands.recv() {
        match command {
            AsrCommand::Load(paths) => {
                let result = backend.load(paths);
                model_loaded = result.is_ok();
                let _ = events.send(AsrEvent::Loaded(result));
            }
            AsrCommand::Transcribe(samples) => {
                let result = if model_loaded {
                    backend.transcribe(&samples)
                } else {
                    Err("model is not loaded".to_owned())
                };
                let _ = events.send(AsrEvent::Recognized(
                    result.map(|raw| raw.trim().to_owned()),
                ));
            }
            AsrCommand::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;

    use crate::model::ModelPaths;

    use super::{run_asr_worker, AsrCommand, AsrEvent, RecognizerBackend};

    struct FakeBackend {
        load_result: Result<(), String>,
        transcriptions: VecDeque<Result<String, String>>,
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Default for FakeBackend {
        fn default() -> Self {
            Self {
                load_result: Ok(()),
                transcriptions: VecDeque::new(),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl RecognizerBackend for FakeBackend {
        fn load(&mut self, _paths: ModelPaths) -> Result<(), String> {
            self.calls.lock().unwrap().push("load");
            self.load_result.clone()
        }

        fn transcribe(&mut self, _samples: &[f32]) -> Result<String, String> {
            self.calls.lock().unwrap().push("transcribe");
            self.transcriptions
                .pop_front()
                .unwrap_or_else(|| Err("missing fake transcription".into()))
        }
    }

    fn model_paths() -> ModelPaths {
        ModelPaths::for_test(
            PathBuf::from("encoder.int8.onnx"),
            PathBuf::from("decoder.onnx"),
            PathBuf::from("joiner.onnx"),
            PathBuf::from("tokens.txt"),
        )
    }

    fn run_worker(
        backend: FakeBackend,
        commands_to_send: impl FnOnce(&mpsc::Sender<AsrCommand>),
    ) -> (Vec<AsrEvent>, Arc<Mutex<Vec<&'static str>>>) {
        let calls = Arc::clone(&backend.calls);
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        commands_to_send(&command_sender);
        run_asr_worker(command_receiver, event_sender, backend);
        (event_receiver.try_iter().collect(), calls)
    }

    #[test]
    fn processes_load_then_transcribe_in_command_order() {
        let (events, calls) = run_worker(
            FakeBackend {
                transcriptions: VecDeque::from([Ok("привет".into())]),
                ..Default::default()
            },
            |commands| {
                commands.send(AsrCommand::Load(model_paths())).unwrap();
                commands.send(AsrCommand::Transcribe(vec![0.25])).unwrap();
                commands.send(AsrCommand::Shutdown).unwrap();
            },
        );

        assert_eq!(
            events,
            vec![
                AsrEvent::Loaded(Ok(())),
                AsrEvent::Recognized(Ok("привет".into()))
            ]
        );
        assert_eq!(*calls.lock().unwrap(), ["load", "transcribe"]);
    }

    #[test]
    fn reports_load_failure() {
        let (events, _) = run_worker(
            FakeBackend {
                load_result: Err("broken model".into()),
                ..Default::default()
            },
            |commands| {
                commands.send(AsrCommand::Load(model_paths())).unwrap();
                commands.send(AsrCommand::Shutdown).unwrap();
            },
        );

        assert_eq!(events, vec![AsrEvent::Loaded(Err("broken model".into()))]);
    }

    #[test]
    fn rejects_transcription_before_successful_load() {
        let (events, calls) = run_worker(FakeBackend::default(), |commands| {
            commands.send(AsrCommand::Transcribe(vec![0.25])).unwrap();
            commands.send(AsrCommand::Shutdown).unwrap();
        });

        assert_eq!(
            events,
            vec![AsrEvent::Recognized(Err("model is not loaded".into()))]
        );
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn preserves_empty_recognition_output() {
        let (events, _) = run_worker(
            FakeBackend {
                transcriptions: VecDeque::from([Ok(String::new())]),
                ..Default::default()
            },
            |commands| {
                commands.send(AsrCommand::Load(model_paths())).unwrap();
                commands.send(AsrCommand::Transcribe(vec![])).unwrap();
                commands.send(AsrCommand::Shutdown).unwrap();
            },
        );

        assert_eq!(events[1], AsrEvent::Recognized(Ok(String::new())));
    }

    #[test]
    fn trims_recognition_output() {
        let (events, _) = run_worker(
            FakeBackend {
                transcriptions: VecDeque::from([Ok(" \n привет \t ".into())]),
                ..Default::default()
            },
            |commands| {
                commands.send(AsrCommand::Load(model_paths())).unwrap();
                commands.send(AsrCommand::Transcribe(vec![])).unwrap();
                commands.send(AsrCommand::Shutdown).unwrap();
            },
        );

        assert_eq!(events[1], AsrEvent::Recognized(Ok("привет".into())));
    }

    #[test]
    fn shuts_down_cleanly() {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, _event_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            run_asr_worker(command_receiver, event_sender, FakeBackend::default())
        });

        command_sender.send(AsrCommand::Shutdown).unwrap();

        worker.join().unwrap();
    }
}
