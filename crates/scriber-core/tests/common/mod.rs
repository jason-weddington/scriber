//! Fakes for every port, gathered in a [`World`] that lends them out as the
//! core's [`Ports`]. Each fake records what it was asked and can be told to
//! fail, so a test sets up exactly the world it needs.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read};

use scriber_core::dictation::{Job, Outcome};
use scriber_core::ports::{
    Captured, Clipboard, Downloader, Files, JobQueue, PortError, Ports, Recorder, Transcriber,
};
use scriber_core::store::{VOCABULARY, segment_index, segment_path};

/// `n` samples of a loud square wave (RMS 0.5): never silent.
pub(crate) fn speech(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
        .collect()
}

/// `seconds` of speech at 16 kHz mono, as the recorder would capture it.
pub(crate) fn captured_speech(seconds: usize) -> Captured {
    Captured::new(speech(seconds.saturating_mul(16_000)), 1, 16_000)
}

/// A microphone. Each `stop` hands over the next queued capture (one second
/// of speech when the queue is empty).
#[derive(Default)]
pub(crate) struct FakeRecorder {
    pub(crate) captures: RefCell<VecDeque<Result<Captured, String>>>,
    pub(crate) start_error: RefCell<Option<String>>,
    pub(crate) calls: RefCell<Vec<&'static str>>,
}

impl Recorder for FakeRecorder {
    fn start(&self) -> Result<(), PortError> {
        self.calls.borrow_mut().push("start");
        match self.start_error.borrow().clone() {
            Some(e) => Err(PortError(e)),
            None => Ok(()),
        }
    }

    fn stop(&self) -> Result<Captured, PortError> {
        self.calls.borrow_mut().push("stop");
        self.captures
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Ok(captured_speech(1)))
            .map_err(PortError)
    }
}

/// A transcriber that answers from a queue ("segment N" when it is empty)
/// and records every call's sample count and prompt, and every warm-up.
#[derive(Default)]
pub(crate) struct FakeTranscriber {
    pub(crate) missing: RefCell<Option<String>>,
    pub(crate) answers: RefCell<VecDeque<Result<String, String>>>,
    pub(crate) calls: RefCell<Vec<(usize, Option<String>)>>,
    pub(crate) warms: Cell<u32>,
    pub(crate) warm_error: RefCell<Option<String>>,
}

impl Transcriber for FakeTranscriber {
    fn missing_model(&self) -> Option<String> {
        self.missing.borrow().clone()
    }

    fn warm(&self) -> Result<(), PortError> {
        self.warms.set(self.warms.get().saturating_add(1));
        match self.warm_error.borrow().clone() {
            Some(e) => Err(PortError(e)),
            None => Ok(()),
        }
    }

    fn transcribe(&self, samples: &[f32], prompt: Option<&str>) -> Result<String, PortError> {
        let n = self.calls.borrow().len();
        self.calls
            .borrow_mut()
            .push((samples.len(), prompt.map(str::to_string)));
        self.answers
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Ok(format!(" segment {n} ")))
            .map_err(PortError)
    }

    fn unload(&self) -> bool {
        false
    }
}

/// A data folder held in a map from path to contents, with a switch to make
/// each operation fail.
#[derive(Default)]
pub(crate) struct FakeFiles {
    pub(crate) files: RefCell<BTreeMap<String, Vec<u8>>>,
    pub(crate) fail_write: Cell<bool>,
    pub(crate) fail_read: Cell<bool>,
    pub(crate) fail_list: Cell<bool>,
    pub(crate) fail_remove: Cell<bool>,
    pub(crate) fail_append: Cell<bool>,
    pub(crate) fail_rename: Cell<bool>,
}

impl FakeFiles {
    /// Segment `index`'s bytes, if stored.
    pub(crate) fn segment(&self, index: u32) -> Option<Vec<u8>> {
        self.files.borrow().get(&segment_path(index)).cloned()
    }

    /// Stores segment `index` directly, as a previous run would have.
    pub(crate) fn put_segment(&self, index: u32, wav: Vec<u8>) {
        let _: Option<_> = self.files.borrow_mut().insert(segment_path(index), wav);
    }

    /// How many segments are stored.
    pub(crate) fn segments(&self) -> usize {
        let files = self.files.borrow();
        files
            .keys()
            .filter_map(|k| k.strip_prefix("segments/"))
            .filter(|name| segment_index(name).is_some())
            .count()
    }

    /// Writes (or with `None` deletes) the glossary.
    pub(crate) fn set_vocabulary(&self, text: Option<&str>) {
        let mut files = self.files.borrow_mut();
        let _: Option<_> = match text {
            Some(t) => files.insert(VOCABULARY.to_string(), t.as_bytes().to_vec()),
            None => files.remove(VOCABULARY),
        };
    }
}

impl Files for FakeFiles {
    fn read(&self, path: &str) -> Result<Vec<u8>, PortError> {
        if self.fail_read.get() {
            return Err(PortError("permission denied".to_string()));
        }
        self.files
            .borrow()
            .get(path)
            .cloned()
            .ok_or_else(|| PortError("no such file".to_string()))
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), PortError> {
        if self.fail_write.get() {
            return Err(PortError("disk full".to_string()));
        }
        let _: Option<_> = self
            .files
            .borrow_mut()
            .insert(path.to_string(), bytes.to_vec());
        Ok(())
    }

    fn list(&self, folder: &str) -> Result<Vec<String>, PortError> {
        if self.fail_list.get() {
            return Err(PortError("permission denied".to_string()));
        }
        let prefix = format!("{folder}/");
        // Reversed, to prove the core does not rely on the order.
        Ok(self
            .files
            .borrow()
            .keys()
            .rev()
            .filter_map(|k| k.strip_prefix(&prefix).map(str::to_string))
            .collect())
    }

    fn remove(&self, path: &str) -> Result<(), PortError> {
        if self.fail_remove.get() {
            return Err(PortError("busy".to_string()));
        }
        let _: Option<_> = self.files.borrow_mut().remove(path);
        Ok(())
    }

    fn append(&self, path: &str, bytes: &[u8]) -> Result<(), PortError> {
        if self.fail_append.get() {
            return Err(PortError("disk full".to_string()));
        }
        let mut files = self.files.borrow_mut();
        let file = files
            .get_mut(path)
            .ok_or_else(|| PortError("no such file".to_string()))?;
        file.extend_from_slice(bytes);
        Ok(())
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), PortError> {
        if self.fail_rename.get() {
            return Err(PortError("read-only volume".to_string()));
        }
        let mut files = self.files.borrow_mut();
        let bytes = files
            .remove(from)
            .ok_or_else(|| PortError("no such file".to_string()))?;
        let _: Option<_> = files.insert(to.to_string(), bytes);
        Ok(())
    }
}

/// A network that serves `blocks` for any URL, one per read, then fails the
/// read with `error` if it is set; refuses the request with `refuse`; and
/// records every URL asked for. `interrupt` makes the first read fail with
/// `Interrupted`, which a reader is expected to retry.
#[derive(Default)]
pub(crate) struct FakeDownloader {
    pub(crate) blocks: RefCell<Vec<Vec<u8>>>,
    pub(crate) error: RefCell<Option<String>>,
    pub(crate) refuse: RefCell<Option<String>>,
    pub(crate) interrupt: Cell<bool>,
    pub(crate) urls: RefCell<Vec<String>>,
}

/// The body [`FakeDownloader`] answers with.
struct FakeBody(VecDeque<io::Result<Vec<u8>>>);

impl Read for FakeBody {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let Some(next) = self.0.pop_front() else {
            return Ok(0);
        };
        let bytes = next?;
        // Every fake block fits the reader's buffer.
        buffer
            .iter_mut()
            .zip(&bytes)
            .for_each(|(to, from)| *to = *from);
        Ok(bytes.len().min(buffer.len()))
    }
}

impl Downloader for FakeDownloader {
    fn open(&self, url: &str) -> Result<Box<dyn Read>, PortError> {
        self.urls.borrow_mut().push(url.to_string());
        if let Some(e) = self.refuse.borrow().clone() {
            return Err(PortError(e));
        }
        let mut reads: VecDeque<io::Result<Vec<u8>>> = VecDeque::new();
        if self.interrupt.get() {
            reads.push_back(Err(io::ErrorKind::Interrupted.into()));
        }
        reads.extend(self.blocks.borrow().iter().cloned().map(Ok));
        if let Some(e) = self.error.borrow().clone() {
            reads.push_back(Err(io::Error::other(e)));
        }
        Ok(Box::new(FakeBody(reads)))
    }
}

/// Records every write; fails while `fail` is set.
#[derive(Default)]
pub(crate) struct FakeClipboard {
    pub(crate) writes: RefCell<Vec<String>>,
    pub(crate) fail: Cell<bool>,
}

impl Clipboard for FakeClipboard {
    fn write(&self, text: &str) -> Result<(), PortError> {
        if self.fail.get() {
            return Err(PortError("pasteboard unavailable".to_string()));
        }
        self.writes.borrow_mut().push(text.to_string());
        Ok(())
    }
}

/// Holds every queued job; refuses them while `fail` is set.
#[derive(Default)]
pub(crate) struct FakeQueue {
    pub(crate) jobs: RefCell<Vec<Job>>,
    pub(crate) fail: Cell<bool>,
}

impl JobQueue for FakeQueue {
    fn send(&self, job: Job) -> Result<(), PortError> {
        if self.fail.get() {
            return Err(PortError("the worker stopped".to_string()));
        }
        self.jobs.borrow_mut().push(job);
        Ok(())
    }
}

/// Every fake, owned together.
#[derive(Default)]
pub(crate) struct World {
    pub(crate) recorder: FakeRecorder,
    pub(crate) transcriber: FakeTranscriber,
    pub(crate) files: FakeFiles,
    pub(crate) clipboard: FakeClipboard,
    pub(crate) queue: FakeQueue,
    pub(crate) downloader: FakeDownloader,
    /// Every progress report a job made through [`World::run`].
    pub(crate) progress: RefCell<Vec<u64>>,
}

impl World {
    pub(crate) fn ports(&self) -> Ports<'_> {
        Ports::new(
            &self.recorder,
            &self.transcriber,
            &self.files,
            &self.clipboard,
            &self.queue,
            &self.downloader,
        )
    }

    /// Runs `job` against the fakes, keeping its progress reports.
    pub(crate) fn run(&self, job: &Job) -> Outcome {
        job.run(&self.ports(), &|bytes| {
            self.progress.borrow_mut().push(bytes);
        })
    }
}
