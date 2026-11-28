//! Audio playback: a mixer thread renders the timeline ahead of the sound
//! device, whose callback pulls from the buffer. The number of samples the
//! device consumed is the playback clock that video follows.
use framework_core::{Project, Time};
use framework_media::{AudioMixer, OUTPUT_CHANNELS};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// Frames mixed per step.
const CHUNK: usize = 1024;

#[derive(Default)]
struct Inner {
    generation: u64,
    /// `None` while stopped.
    project: Option<Arc<Project>>,
    /// Timeline sample where playback started.
    start: i64,
    produced: i64,
    consumed: i64,
    buffer: VecDeque<f32>,
    error: Option<String>,
    quit: bool,
    /// The mixer thread is waiting without any open media files.
    idle: bool,
    /// Frames between the device callback and the speaker.
    latency: i64,
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
}

/// Device-independent playback engine; [`AudioOutput`] connects it to the sound card.
pub struct AudioEngine {
    shared: Arc<Shared>,
    rate: u32,
    thread: Option<JoinHandle<()>>,
}

impl AudioEngine {
    pub fn new(rate: u32) -> Self {
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner::default()),
            wake: Condvar::new(),
        });
        let worker = shared.clone();
        let ahead = rate as usize / 5 * OUTPUT_CHANNELS;
        let thread = std::thread::spawn(move || {
            let mut mixer = AudioMixer::new(rate);
            let mut chunk = vec![0.0; CHUNK * OUTPUT_CHANNELS];
            loop {
                let (project, generation, pos) = {
                    let mut g = worker.inner.lock().expect("audio lock");
                    while !g.quit && (g.project.is_none() || g.buffer.len() >= ahead) {
                        if g.project.is_none() && !g.idle {
                            mixer.release();
                            g.idle = true;
                            worker.wake.notify_all();
                        }
                        g = worker.wake.wait(g).expect("audio lock");
                    }
                    if g.quit {
                        return;
                    }
                    let project = g.project.clone().expect("checked above");
                    g.idle = false;
                    (project, g.generation, g.start + g.produced)
                };
                chunk.fill(0.0);
                let result = mixer.mix(&project, pos, &mut chunk);
                let mut g = worker.inner.lock().expect("audio lock");
                if g.generation != generation {
                    continue;
                }
                match result {
                    Ok(()) => {
                        g.buffer.extend(&chunk);
                        g.produced += CHUNK as i64;
                    }
                    Err(e) => {
                        g.error = Some(e.to_string());
                        g.project = None;
                    }
                }
            }
        });
        Self {
            shared,
            rate,
            thread: Some(thread),
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    fn update(&self, f: impl FnOnce(&mut Inner)) {
        let mut g = self.shared.inner.lock().expect("audio lock");
        g.generation += 1;
        g.buffer.clear();
        g.produced = 0;
        g.consumed = 0;
        f(&mut g);
        self.shared.wake.notify_all();
    }

    /// Starts playing `project` from timeline time `t`.
    pub fn play(&self, project: &Project, t: Time) {
        let start = t.to_samples(self.rate);
        let project = Arc::new(project.clone());
        self.update(|g| {
            g.start = start;
            g.project = Some(project);
        });
    }

    pub fn stop(&self) {
        self.update(|g| g.project = None);
    }

    /// Stops and waits until the mixer has closed its media files, e.g.
    /// before the project file that embeds them is replaced.
    pub fn release(&self) {
        self.stop();
        let mut g = self.shared.inner.lock().expect("audio lock");
        while !g.idle && !g.quit {
            g = self.shared.wake.wait(g).expect("audio lock");
        }
    }

    /// Timeline time of the sample the device is playing, while playing.
    pub fn position(&self) -> Option<Time> {
        let g = self.shared.inner.lock().expect("audio lock");
        g.project
            .as_ref()
            .map(|_| Time::from_samples(g.start + (g.consumed - g.latency).max(0), self.rate))
    }

    /// Output latency reported by the device; the clock lags by it so the
    /// picture matches what is heard.
    pub fn set_latency(&self, latency: std::time::Duration) {
        let frames = (latency.as_secs_f64() * f64::from(self.rate)) as i64;
        self.shared.inner.lock().expect("audio lock").latency = frames;
    }

    /// A mixing error since the last call; playback stops on errors.
    pub fn take_error(&self) -> Option<String> {
        self.shared.inner.lock().expect("audio lock").error.take()
    }

    /// Fills an interleaved device buffer with `channels` channels; silence on underrun.
    pub fn pull(&self, out: &mut [f32], channels: usize) {
        let mut g = self.shared.inner.lock().expect("audio lock");
        for frame in out.chunks_mut(channels.max(1)) {
            frame.fill(0.0);
            if g.buffer.len() < OUTPUT_CHANNELS {
                continue;
            }
            let (l, r) = (
                g.buffer.pop_front().unwrap_or(0.0),
                g.buffer.pop_front().unwrap_or(0.0),
            );
            match frame {
                [mono] => *mono = (l + r) * 0.5,
                [left, right, ..] => (*left, *right) = (l, r),
                [] => {}
            }
            g.consumed += 1;
        }
        self.shared.wake.notify_all();
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.shared.inner.lock().expect("audio lock").quit = true;
        self.shared.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The engine connected to the default output device.
pub struct AudioOutput {
    pub engine: Arc<AudioEngine>,
    _stream: cpal::Stream,
}

impl AudioOutput {
    /// Opens the default output device; `None` when there is none (e.g. CI).
    pub fn open() -> Option<Self> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let device = cpal::default_host().default_output_device()?;
        let supported = device.default_output_config().ok()?;
        let config = supported.config();
        let engine = Arc::new(AudioEngine::new(config.sample_rate));
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, engine.clone()),
            cpal::SampleFormat::F64 => build::<f64>(&device, &config, engine.clone()),
            cpal::SampleFormat::I8 => build::<i8>(&device, &config, engine.clone()),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, engine.clone()),
            cpal::SampleFormat::I24 => build::<cpal::I24>(&device, &config, engine.clone()),
            cpal::SampleFormat::I32 => build::<i32>(&device, &config, engine.clone()),
            cpal::SampleFormat::U8 => build::<u8>(&device, &config, engine.clone()),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, engine.clone()),
            cpal::SampleFormat::U32 => build::<u32>(&device, &config, engine.clone()),
            _ => None,
        }?;
        stream.play().ok()?;
        Some(Self {
            engine,
            _stream: stream,
        })
    }
}

fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    engine: Arc<AudioEngine>,
) -> Option<cpal::Stream> {
    use cpal::traits::DeviceTrait;
    let channels = usize::from(config.channels);
    let mut scratch = Vec::new();
    device
        .build_output_stream(
            *config,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                let ts = info.timestamp();
                engine.set_latency(ts.playback.duration_since(ts.callback));
                scratch.resize(data.len(), 0.0);
                engine.pull(&mut scratch, channels);
                for (d, s) in data.iter_mut().zip(&scratch) {
                    *d = T::from_sample(*s);
                }
            },
            |e| tracing::warn!("audio output: {e}"),
            None,
        )
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use framework_core::{MediaAsset, MediaKind};

    fn project_with_tone() -> Project {
        let dir = std::env::temp_dir().join(format!("framework-audio-out-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        framework_media::synthetic::write_test_wav(&wav, 8000, 1, 1.0, 440).unwrap();
        let mut p = Project::new("t");
        let info = framework_media::probe(&wav).unwrap();
        let id = p.add_media(MediaAsset::new(wav, info));
        let track = p.default_track_for(MediaKind::Audio);
        p.append_media(track, id).unwrap();
        p
    }

    fn pull_until_audible(engine: &AudioEngine, channels: usize) -> Vec<f32> {
        let mut out = vec![0.0; 256 * channels];
        for _ in 0..500 {
            engine.pull(&mut out, channels);
            if out.iter().any(|s| *s != 0.0) {
                return out;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("no audio produced");
    }

    #[test]
    fn plays_from_position_and_tracks_the_clock() {
        let p = project_with_tone();
        let engine = AudioEngine::new(8000);
        assert_eq!(engine.rate(), 8000);
        assert_eq!(engine.position(), None);
        let mut silent = vec![1.0; 8];
        engine.pull(&mut silent, 2);
        assert!(silent.iter().all(|s| *s == 0.0));

        engine.play(&p, Time::from_seconds(0.25));
        let stereo = pull_until_audible(&engine, 2);
        assert!(stereo.chunks(2).any(|f| f[0] != 0.0 && f[0] == f[1]));
        assert!(engine.position().unwrap() > Time::from_seconds(0.25));

        // Mono and surround devices get a down- or up-mix.
        let mono = pull_until_audible(&engine, 1);
        assert!(mono.iter().any(|s| *s != 0.0));
        let surround = pull_until_audible(&engine, 6);
        assert!(surround.chunks(6).all(|f| f[2..].iter().all(|s| *s == 0.0)));

        // The clock lags by the device latency.
        let before = engine.position().unwrap();
        engine.set_latency(std::time::Duration::from_millis(10));
        assert_eq!(
            before - engine.position().unwrap(),
            Time::from_samples(80, 8000)
        );

        engine.stop();
        assert_eq!(engine.position(), None);
        assert_eq!(engine.take_error(), None);
    }

    #[test]
    fn release_waits_until_media_is_closed() {
        let p = project_with_tone();
        let engine = AudioEngine::new(8000);
        engine.release();
        engine.play(&p, Time::ZERO);
        pull_until_audible(&engine, 2);
        engine.release();
        assert_eq!(engine.position(), None);
        assert!(engine.shared.inner.lock().unwrap().idle);
    }

    #[test]
    fn mixing_errors_stop_playback() {
        let mut p = project_with_tone();
        p.media[0].path = "/no/such/file.wav".into();
        let engine = AudioEngine::new(8000);
        engine.play(&p, Time::ZERO);
        let error = loop {
            if let Some(e) = engine.take_error() {
                break e;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        assert!(error.contains("file.wav"), "{error}");
        assert_eq!(engine.position(), None);
    }

    #[test]
    fn opening_the_device_never_panics() {
        let _ = AudioOutput::open();
    }
}
