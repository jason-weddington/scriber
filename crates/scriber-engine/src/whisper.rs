//! Speech to text: whisper.cpp through whisper-rs.

use std::path::{Path, PathBuf};

use scriber_core::ports::{Phrase, PortError, Transcriber, fail};
use scriber_core::sync::LoadOnce;
use scriber_core::transcript::samples_at;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperSegment,
    WhisperVadContext, WhisperVadContextParams, WhisperVadParams,
};

use crate::extract::VadSettings;

/// The beam width of every decode.
pub(crate) const BEAM_SIZE: i32 = 5;
/// The beam search patience of every decode (whisper.cpp's "unset").
pub(crate) const BEAM_PATIENCE: f32 = -1.0;
/// The language every decode is forced to.
pub(crate) const LANGUAGE: &str = "en";

/// whisper.cpp, on Metal on macOS and on the CPU elsewhere; the model loads
/// once (warm-up or first use) and stays until `unload`.
pub struct Whisper(Source, LoadOnce<WhisperContext>);

/// Where [`Whisper`] looks for its model.
enum Source {
    /// A fixed file.
    Fixed(PathBuf),
    /// `store::model_path` under this data folder.
    DataDir(PathBuf),
}

impl Source {
    /// The model file.
    fn resolve(&self) -> PathBuf {
        match self {
            Self::Fixed(path) => path.clone(),
            Self::DataDir(data_dir) => data_dir.join(scriber_core::store::model_path(
                &crate::DataDir::new(data_dir.clone()),
            )),
        }
    }
}

/// One Whisper segment with everything the decoder reports about it, for
/// measurement (the study-104 extractor); the app uses only [`Phrase`].
#[derive(Debug, Clone, PartialEq)]
pub struct DetailedPhrase {
    /// The text and span, exactly as [`Transcriber::transcribe`] gives them.
    pub phrase: Phrase,
    /// Whisper's probability that the segment holds no speech.
    pub no_speech_probability: f32,
    /// Every token of the segment, in order, special ones included.
    pub tokens: Vec<DetailedToken>,
}

/// One token of a [`DetailedPhrase`].
#[derive(Debug, Clone, PartialEq)]
pub struct DetailedToken {
    /// The token id in the model's vocabulary.
    pub id: i32,
    /// True for a special token (end of text, timestamps and the like): an
    /// id at or above the model's end-of-text token.
    pub special: bool,
    /// The token's text, lossily decoded.
    pub text: String,
    /// The token's probability.
    pub probability: f32,
    /// The token's log probability.
    pub log_probability: f32,
}

impl Whisper {
    /// A transcriber for the ggml model at `path`. Loads nothing: the
    /// warm-up or the first transcription does.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        whisper_rs::install_logging_hooks();
        Self(Source::Fixed(path), LoadOnce::default())
    }

    /// A transcriber for the model in `data_dir`, found through
    /// `scriber_core::store::model_path`. `store::model_path` answers an
    /// absolute path when `settings.json`'s `model_folder` is set, and
    /// `Path::join` replaces the base with an absolute argument, so the
    /// configured folder wins; when it is unset the relative
    /// `models/ggml-large-v3-turbo.bin` is joined onto the data folder — the
    /// same file `download::fetch` writes, because `DataDir::write` also does
    /// `root.join(path)` (`src/files.rs:34-38`).
    #[must_use]
    pub fn in_data_dir(data_dir: PathBuf) -> Self {
        whisper_rs::install_logging_hooks();
        Self(Source::DataDir(data_dir), LoadOnce::default())
    }

    fn load(&self) -> Result<WhisperContext, PortError> {
        let params = WhisperContextParameters::default();
        WhisperContext::new_with_params(self.0.resolve(), params).map_err(fail)
    }

    /// Transcribes `samples` with the same parameters as
    /// [`Transcriber::transcribe`] (and the same load-once model), reporting
    /// each phrase's no-speech probability and its tokens as well.
    ///
    /// # Errors
    ///
    /// The model cannot be loaded, or whisper.cpp fails.
    pub fn transcribe_detailed(
        &self,
        samples: &[f32],
        prompt: Option<&str>,
    ) -> Result<Vec<DetailedPhrase>, PortError> {
        self.1.with(&mut || self.load(), &mut |m| {
            decode_detailed(m, samples, prompt)
        })
    }
}

impl Transcriber for Whisper {
    fn missing_model(&self) -> Option<String> {
        let path = self.0.resolve();
        (!path.is_file()).then(|| path.display().to_string())
    }

    fn warm(&self) -> Result<(), PortError> {
        self.1.with(&mut || self.load(), &mut |_| Ok(()))
    }

    fn transcribe(&self, samples: &[f32], prompt: Option<&str>) -> Result<Vec<Phrase>, PortError> {
        self.1
            .with(&mut || self.load(), &mut |m| decode(m, samples, prompt))
    }

    fn unload(&self) -> bool {
        self.1.close()
    }
}

/// The parameters of every decode: beam search (beam 5), English,
/// non-speech tokens suppressed, and the prompt when there is one.
fn full_params(prompt: Option<&str>) -> FullParams<'_, '_> {
    let beam = SamplingStrategy::BeamSearch {
        beam_size: BEAM_SIZE,
        patience: BEAM_PATIENCE,
    };
    let mut params = FullParams::new(beam);
    params.set_language(Some(LANGUAGE));
    params.set_suppress_nst(true);
    if let Some(prompt) = prompt {
        params.set_initial_prompt(prompt);
    }
    params
}

/// The text and span of one Whisper segment.
fn phrase(s: &WhisperSegment<'_>) -> Result<Phrase, PortError> {
    let text = s.to_str_lossy().map_err(fail)?;
    Ok(Phrase {
        text: text.into_owned(),
        start: samples_at(s.start_timestamp()),
        end: samples_at(s.end_timestamp()),
    })
}

fn decode(
    model: &WhisperContext,
    pcm: &[f32],
    prompt: Option<&str>,
) -> Result<Vec<Phrase>, PortError> {
    let mut state = model.create_state().map_err(fail)?;
    state.full(full_params(prompt), pcm).map_err(fail)?;
    state.as_iter().map(|s| phrase(&s)).collect()
}

fn decode_detailed(
    model: &WhisperContext,
    pcm: &[f32],
    prompt: Option<&str>,
) -> Result<Vec<DetailedPhrase>, PortError> {
    let eot = model.token_eot();
    let mut state = model.create_state().map_err(fail)?;
    state.full(full_params(prompt), pcm).map_err(fail)?;
    state
        .as_iter()
        .map(|s| {
            let tokens = (0..s.n_tokens())
                .map(|i| {
                    let token = s
                        .get_token(i)
                        .ok_or_else(|| PortError(format!("token {i} out of range")))?;
                    let data = token.token_data();
                    Ok(DetailedToken {
                        id: data.id,
                        special: data.id >= eot,
                        text: token.to_str_lossy().map_err(fail)?.into_owned(),
                        probability: token.token_probability(),
                        log_probability: data.plog,
                    })
                })
                .collect::<Result<Vec<_>, PortError>>()?;
            Ok(DetailedPhrase {
                phrase: phrase(&s)?,
                no_speech_probability: s.no_speech_probability(),
                tokens,
            })
        })
        .collect()
}

/// The speech spans (start and end in seconds) that the Silero VAD model at
/// `model_path` finds in `samples` (16 kHz mono), run standalone through
/// whisper.cpp's VAD context with `settings`; the maximum speech duration,
/// the samples overlap, the thread count and the GPU use stay at whisper-rs's
/// defaults. Each call loads the model and starts from a fresh state.
///
/// # Errors
///
/// The path is not UTF-8 or holds a NUL byte, the model cannot be loaded,
/// or whisper.cpp fails.
pub fn vad_spans(
    model_path: &Path,
    samples: &[f32],
    settings: &VadSettings,
) -> Result<Vec<(f64, f64)>, PortError> {
    let path = model_path
        .to_str()
        .filter(|p| !p.contains('\0'))
        .ok_or_else(|| PortError(format!("unusable VAD model path {}", model_path.display())))?;
    let mut ctx = WhisperVadContext::new(path, WhisperVadContextParams::new()).map_err(fail)?;
    let mut params = WhisperVadParams::new();
    params.set_threshold(settings.threshold);
    params.set_min_silence_duration(settings.min_silence_duration_ms);
    params.set_speech_pad(settings.speech_pad_ms);
    params.set_min_speech_duration(settings.min_speech_duration_ms);
    let segments = ctx.segments_from_samples(params, samples).map_err(fail)?;
    // whisper.cpp reports VAD timestamps in centiseconds.
    Ok(segments
        .map(|s| (f64::from(s.start) / 100.0, f64::from(s.end) / 100.0))
        .collect())
}
