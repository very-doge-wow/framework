use crate::MediaError;
use ffmpeg_next::{
    ChannelLayout, codec, format, frame, media, software::resampling, util::format::sample::Type,
};
use framework_core::Time;
use std::collections::VecDeque;
use std::path::Path;

/// Number of interleaved output channels produced by [`AudioReader`].
pub const OUTPUT_CHANNELS: usize = 2;

/// Decodes the best audio stream of a file into interleaved stereo `f32`
/// samples at a fixed output rate.
pub struct AudioReader {
    input: format::context::Input,
    stream_index: usize,
    decoder: codec::decoder::Audio,
    time_base: (i32, i32),
    start_pts: i64,
    rate: u32,
    resampler: Option<(resampling::Context, (format::Sample, u64, u32))>,
    /// Interleaved samples; `buffer_start` is the source sample index of the first frame.
    buffer: VecDeque<f32>,
    buffer_start: Option<i64>,
    eof: bool,
}

impl AudioReader {
    pub fn open(path: &Path, rate: u32) -> Result<Self, MediaError> {
        crate::init();
        let input = format::input(path).map_err(|source| MediaError::Open {
            path: path.to_owned(),
            source,
        })?;
        let stream = input
            .streams()
            .best(media::Type::Audio)
            .ok_or_else(|| MediaError::NoAudio(path.to_owned()))?;
        let stream_index = stream.index();
        let tb = stream.time_base();
        let start_pts = if stream.start_time() == ffmpeg_next::ffi::AV_NOPTS_VALUE {
            0
        } else {
            stream.start_time()
        };
        let decoder = codec::context::Context::from_parameters(stream.parameters())?
            .decoder()
            .audio()?;
        Ok(Self {
            input,
            stream_index,
            decoder,
            time_base: (tb.numerator(), tb.denominator()),
            start_pts,
            rate,
            resampler: None,
            buffer: VecDeque::new(),
            buffer_start: None,
            eof: false,
        })
    }

    fn buffered_frames(&self) -> i64 {
        (self.buffer.len() / OUTPUT_CHANNELS) as i64
    }

    fn seek(&mut self, sample: i64) -> Result<(), MediaError> {
        let start = Time::from_timebase(self.start_pts, self.time_base.0, self.time_base.1);
        let ts = (Time::from_samples(sample, self.rate) + start)
            .to_timebase(1, ffmpeg_next::ffi::AV_TIME_BASE);
        self.input.seek(ts, ..ts)?;
        self.decoder.flush();
        self.resampler = None;
        self.buffer.clear();
        self.buffer_start = None;
        self.eof = false;
        Ok(())
    }

    fn resample_into_buffer(&mut self, decoded: &mut frame::Audio) -> Result<(), MediaError> {
        let mut layout = decoded.channel_layout();
        if layout.is_empty()
            || layout.0.order != ffmpeg_next::ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE
        {
            layout = ChannelLayout::default(i32::from(decoded.channels()));
            decoded.set_channel_layout(layout);
        }
        let key = (decoded.format(), layout.bits(), decoded.rate());
        if self.resampler.as_ref().is_none_or(|(_, k)| *k != key) {
            let ctx = resampling::Context::get(
                decoded.format(),
                layout,
                decoded.rate(),
                format::Sample::F32(Type::Packed),
                ChannelLayout::STEREO,
                self.rate,
            )?;
            self.resampler = Some((ctx, key));
        }
        let (ctx, _) = self
            .resampler
            .as_mut()
            .expect("resampler initialised above");
        let mut out = frame::Audio::empty();
        ctx.run(decoded, &mut out)?;
        let n = out.samples() * OUTPUT_CHANNELS;
        if n > 0 {
            self.buffer.extend(&out.plane::<f32>(0)[..n]);
        }
        Ok(())
    }

    /// Decodes one more chunk into the buffer. Returns `false` at end of stream.
    fn fill(&mut self) -> Result<bool, MediaError> {
        loop {
            let mut decoded = frame::Audio::empty();
            if self.decoder.receive_frame(&mut decoded).is_ok() {
                if self.buffer_start.is_none() {
                    let pts = decoded.timestamp().unwrap_or(self.start_pts);
                    let t = Time::from_timebase(
                        pts - self.start_pts,
                        self.time_base.0,
                        self.time_base.1,
                    );
                    self.buffer_start = Some(t.to_samples(self.rate));
                }
                self.resample_into_buffer(&mut decoded)?;
                return Ok(true);
            }
            if self.eof {
                return Ok(false);
            }
            match self.input.packets().next() {
                Some((stream, packet)) => {
                    if stream.index() == self.stream_index {
                        self.decoder.send_packet(&packet)?;
                    }
                }
                None => {
                    self.eof = true;
                    self.decoder.send_eof()?;
                }
            }
        }
    }

    /// Adds `gain`-scaled samples starting at source sample `start` into
    /// `out` (interleaved stereo). Missing samples (past the end) are left untouched.
    pub fn mix_into(&mut self, start: i64, out: &mut [f32], gain: f32) -> Result<(), MediaError> {
        let frames = (out.len() / OUTPUT_CHANNELS) as i64;
        let needs_seek = match self.buffer_start {
            Some(bs) => start < bs || start > bs + self.buffered_frames() + i64::from(self.rate),
            None => start > 0,
        };
        if needs_seek {
            self.seek(start)?;
        }
        while self
            .buffer_start
            .is_none_or(|bs| bs + self.buffered_frames() < start + frames)
        {
            if !self.fill()? {
                break;
            }
        }
        let Some(bs) = self.buffer_start else {
            return Ok(());
        };
        if start > bs {
            let drop = ((start - bs) as usize * OUTPUT_CHANNELS).min(self.buffer.len());
            self.buffer.drain(..drop);
            self.buffer_start = Some(bs + (drop / OUTPUT_CHANNELS) as i64);
        }
        let bs = self.buffer_start.unwrap_or(start);
        let lead = ((bs - start).max(0) as usize * OUTPUT_CHANNELS).min(out.len());
        for (o, s) in out[lead..].iter_mut().zip(self.buffer.iter()) {
            *o += *s * gain;
        }
        Ok(())
    }
}
