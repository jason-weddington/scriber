//! Speech to text: whisper.cpp through whisper-rs.

use std::path::PathBuf;

use scriber_core::ports::{PortError, Transcriber, fail};
use scriber_core::sync::LoadOnce;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// whisper.cpp, on Metal on macOS and on the CPU elsewhere; the model loads
/// once (warm-up or first use) and stays.
pub struct Whisper(PathBuf, LoadOnce<WhisperContext>);

impl Whisper {
    /// A transcriber for the ggml model at `path`. Loads nothing: the
    /// warm-up or the first transcription does.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        whisper_rs::install_logging_hooks();
        Self(path, LoadOnce::default())
    }

    fn load(&self) -> Result<WhisperContext, PortError> {
        let params = WhisperContextParameters::default();
        WhisperContext::new_with_params(&self.0, params).map_err(fail)
    }
}

impl Transcriber for Whisper {
    fn missing_model(&self) -> Option<String> {
        (!self.0.is_file()).then(|| self.0.display().to_string())
    }

    fn warm(&self) -> Result<(), PortError> {
        self.1.with(&mut || self.load(), &mut |_| Ok(()))
    }

    fn transcribe(&self, samples: &[f32], prompt: Option<&str>) -> Result<String, PortError> {
        self.1
            .with(&mut || self.load(), &mut |m| decode(m, samples, prompt))
    }
}

/// Beam search (beam 5), English, non-speech tokens suppressed.
fn decode(model: &WhisperContext, pcm: &[f32], prompt: Option<&str>) -> Result<String, PortError> {
    let beam = SamplingStrategy::BeamSearch {
        beam_size: 5,
        patience: -1.0,
    };
    let mut params = FullParams::new(beam);
    params.set_language(Some("en"));
    params.set_suppress_nst(true);
    if let Some(prompt) = prompt {
        params.set_initial_prompt(prompt);
    }
    let mut state = model.create_state().map_err(fail)?;
    state.full(params, pcm).map_err(fail)?;
    let text = state.as_iter().map(|s| s.to_str_lossy().map(String::from));
    text.collect::<Result<String, _>>().map_err(fail)
}
