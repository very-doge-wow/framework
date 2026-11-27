use crate::MediaError;
use crate::audio::{AudioReader, OUTPUT_CHANNELS};
use crate::video::{Fitter, VideoReader};
use ffmpeg_next::{
    ChannelLayout, Dictionary, Packet, codec, color, encoder, format, frame,
    util::format::sample::Type,
};
use framework_core::{
    Clip, ClipId, ExportProfile, MediaAsset, MediaKind, Project, Time, TrackKind,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportProgress {
    pub frame: u64,
    pub total_frames: u64,
}

impl ExportProgress {
    pub fn fraction(&self) -> f32 {
        if self.total_frames == 0 {
            1.0
        } else {
            self.frame as f32 / self.total_frames as f32
        }
    }
}

/// An H.264/AAC MP4 writer configured from an [`ExportProfile`].
pub(crate) struct Muxer {
    output: format::context::Output,
    video: encoder::Video,
    video_tb: ffmpeg_next::Rational,
    audio: Option<encoder::Audio>,
    audio_tb: ffmpeg_next::Rational,
    audio_frame_size: usize,
    sample_rate: u32,
}

impl Muxer {
    pub(crate) fn create(
        path: &Path,
        profile: &ExportProfile,
        with_audio: bool,
    ) -> Result<Self, MediaError> {
        crate::init();
        let mut output =
            format::output_as(path, profile.container.extension()).map_err(|source| {
                MediaError::Open {
                    path: path.to_owned(),
                    source,
                }
            })?;
        let global_header = output
            .format()
            .flags()
            .contains(format::Flags::GLOBAL_HEADER);
        let v = &profile.video;
        let a = &profile.audio;

        let vcodec =
            encoder::find_by_name("libx264").ok_or(MediaError::MissingEncoder("libx264"))?;
        let mut venc = codec::context::Context::new_with_codec(vcodec)
            .encoder()
            .video()?;
        let video_tb = ffmpeg_next::Rational(v.frame_rate.den as i32, v.frame_rate.num as i32);
        venc.set_width(v.width);
        venc.set_height(v.height);
        venc.set_format(format::Pixel::YUV420P);
        venc.set_time_base(video_tb);
        venc.set_frame_rate(Some(ffmpeg_next::Rational(
            v.frame_rate.num as i32,
            v.frame_rate.den as i32,
        )));
        venc.set_bit_rate(v.bitrate_kbps as usize * 1000);
        venc.set_max_bit_rate(v.bitrate_kbps as usize * 1500);
        venc.set_gop((v.frame_rate.as_f64() * 2.0).round().max(1.0) as u32);
        venc.set_colorspace(color::Space::BT709);
        venc.set_color_range(color::Range::MPEG);
        if global_header {
            venc.set_flags(codec::Flags::GLOBAL_HEADER);
        }
        let mut opts = Dictionary::new();
        opts.set("preset", &v.preset);
        opts.set("profile", "high");
        opts.set("bufsize", &(v.bitrate_kbps as usize * 2000).to_string());
        let video = venc.open_with(opts)?;

        let mut vst = output.add_stream(vcodec)?;
        vst.set_parameters(&video);
        vst.set_time_base(video_tb);

        let audio_tb = ffmpeg_next::Rational(1, a.sample_rate as i32);
        let audio = if with_audio {
            let acodec = encoder::find(codec::Id::AAC).ok_or(MediaError::MissingEncoder("aac"))?;
            let mut aenc = codec::context::Context::new_with_codec(acodec)
                .encoder()
                .audio()?;
            aenc.set_rate(a.sample_rate as i32);
            aenc.set_channel_layout(ChannelLayout::STEREO);
            aenc.set_format(format::Sample::F32(Type::Planar));
            aenc.set_bit_rate(a.bitrate_kbps as usize * 1000);
            aenc.set_time_base(audio_tb);
            if global_header {
                aenc.set_flags(codec::Flags::GLOBAL_HEADER);
            }
            let audio = aenc.open()?;
            let mut ast = output.add_stream(acodec)?;
            ast.set_parameters(&audio);
            ast.set_time_base(audio_tb);
            Some(audio)
        } else {
            None
        };
        let audio_frame_size = audio
            .as_ref()
            .map_or(1024, |a| a.frame_size() as usize)
            .max(1);

        let mut mux_opts = Dictionary::new();
        mux_opts.set("movflags", "+faststart");
        output.write_header_with(mux_opts)?;

        Ok(Self {
            output,
            video,
            video_tb,
            audio,
            audio_tb,
            audio_frame_size,
            sample_rate: a.sample_rate,
        })
    }

    pub(crate) fn audio_frame_size(&self) -> usize {
        self.audio_frame_size
    }

    fn drain(&mut self, stream: usize) -> Result<(), MediaError> {
        let (enc_tb, out_tb) = {
            let out_tb = self
                .output
                .stream(stream)
                .expect("stream added in create")
                .time_base();
            let enc_tb = if stream == 0 {
                self.video_tb
            } else {
                self.audio_tb
            };
            (enc_tb, out_tb)
        };
        let mut packet = Packet::empty();
        loop {
            let received = if stream == 0 {
                self.video.receive_packet(&mut packet)
            } else {
                match self.audio.as_mut() {
                    Some(audio) => audio.receive_packet(&mut packet),
                    None => return Ok(()),
                }
            };
            if received.is_err() {
                return Ok(());
            }
            packet.set_stream(stream);
            if stream == 0 && packet.duration() <= 0 {
                // One frame in the encoder time base; without it the MP4
                // muxer gives the last sample zero duration and players drop it.
                packet.set_duration(1);
            }
            packet.rescale_ts(enc_tb, out_tb);
            packet.write_interleaved(&mut self.output)?;
        }
    }

    /// Encodes a YUV420P frame of the profile's size as frame number `index`.
    pub(crate) fn write_video(
        &mut self,
        frame: &mut frame::Video,
        index: i64,
    ) -> Result<(), MediaError> {
        frame.set_pts(Some(index));
        self.video.send_frame(frame)?;
        self.drain(0)
    }

    /// Encodes interleaved stereo samples starting at sample `position`.
    /// `samples.len()` must be at most `2 * audio_frame_size()`.
    pub(crate) fn write_audio(&mut self, samples: &[f32], position: i64) -> Result<(), MediaError> {
        let n = samples.len() / OUTPUT_CHANNELS;
        let mut frame =
            frame::Audio::new(format::Sample::F32(Type::Planar), n, ChannelLayout::STEREO);
        frame.set_rate(self.sample_rate);
        for ch in 0..OUTPUT_CHANNELS {
            let plane = frame.plane_mut::<f32>(ch);
            for (i, s) in plane.iter_mut().take(n).enumerate() {
                *s = samples[i * OUTPUT_CHANNELS + ch].clamp(-1.0, 1.0);
            }
        }
        frame.set_pts(Some(position));
        if let Some(audio) = self.audio.as_mut() {
            audio.send_frame(&frame)?;
            self.drain(1)?;
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<(), MediaError> {
        self.video.send_eof()?;
        self.drain(0)?;
        if let Some(audio) = self.audio.as_mut() {
            audio.send_eof()?;
            self.drain(1)?;
        }
        self.output.write_trailer()?;
        Ok(())
    }
}

/// The topmost visible video/image clip at `t`, if any.
pub(crate) fn top_video_clip(project: &Project, t: Time) -> Option<(&Clip, &MediaAsset)> {
    project
        .tracks
        .iter()
        .rev()
        .filter(|tr| tr.kind == TrackKind::Video && !tr.hidden)
        .find_map(|tr| tr.clip_at(t))
        .and_then(|clip| project.media(clip.media).map(|m| (clip, m)))
}

struct AudioMixer<'a> {
    project: &'a Project,
    rate: u32,
    readers: HashMap<ClipId, AudioReader>,
}

impl AudioMixer<'_> {
    /// Mixes all audible clips into `out` (interleaved stereo) starting at timeline sample `s0`.
    fn mix(&mut self, s0: i64, out: &mut [f32]) -> Result<(), MediaError> {
        let n = (out.len() / OUTPUT_CHANNELS) as i64;
        let s1 = s0 + n;
        for track in self.project.tracks.iter().filter(|t| !t.muted) {
            for clip in &track.clips {
                let Some(asset) = self.project.media(clip.media) else {
                    continue;
                };
                if asset.kind() == MediaKind::Image || asset.info.audio.is_none() {
                    continue;
                }
                let cs = clip.start.to_samples(self.rate);
                let ce = clip.end().to_samples(self.rate);
                let (a, b) = (cs.max(s0), ce.min(s1));
                if a >= b {
                    continue;
                }
                let reader = match self.readers.entry(clip.id) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(AudioReader::open(&asset.path, self.rate)?)
                    }
                };
                let src = clip.source_in.to_samples(self.rate) + (a - cs);
                let range =
                    (a - s0) as usize * OUTPUT_CHANNELS..(b - s0) as usize * OUTPUT_CHANNELS;
                reader.mix_into(src, &mut out[range], 1.0)?;
            }
        }
        let project = self.project;
        let rate = self.rate;
        self.readers.retain(|id, _| {
            project
                .tracks
                .iter()
                .flat_map(|t| &t.clips)
                .any(|c| c.id == *id && c.end().to_samples(rate) > s1)
        });
        Ok(())
    }
}

/// Renders the whole project to `output` using `profile`.
///
/// Video tracks are composited by taking the topmost visible clip at each
/// frame; audio from all unmuted tracks is mixed. The file is rendered to
/// `<output>.partial` and only replaces `output` on success.
pub fn export(
    project: &Project,
    profile: &ExportProfile,
    output: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(ExportProgress),
) -> Result<(), MediaError> {
    let mut partial = output.as_os_str().to_owned();
    partial.push(".partial");
    let partial = std::path::PathBuf::from(partial);
    let result = render(project, profile, &partial, cancel, &mut progress)
        .and_then(|()| std::fs::rename(&partial, output).map_err(MediaError::from));
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

fn render(
    project: &Project,
    profile: &ExportProfile,
    output: &Path,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(ExportProgress),
) -> Result<(), MediaError> {
    let duration = project.duration();
    if duration <= Time::ZERO {
        return Err(MediaError::EmptyTimeline);
    }
    let fps = profile.video.frame_rate;
    let rate = profile.audio.sample_rate;
    let mut total_frames = duration.to_frames_floor(fps);
    if Time::from_frames(total_frames, fps) < duration {
        total_frames += 1;
    }
    let total_samples = duration.to_samples(rate);

    let mut muxer = Muxer::create(output, profile, true)?;
    let frame_size = muxer.audio_frame_size() as i64;
    let mut fitter = Fitter::new(
        format::Pixel::YUV420P,
        profile.video.width,
        profile.video.height,
    );
    let mut video_readers: HashMap<ClipId, VideoReader> = HashMap::new();
    let mut mixer = AudioMixer {
        project,
        rate,
        readers: HashMap::new(),
    };
    let mut audio_pos = 0i64;
    let mut samples = Vec::with_capacity(frame_size as usize * OUTPUT_CHANNELS);

    for index in 0..total_frames {
        if cancel.load(Ordering::Relaxed) {
            return Err(MediaError::Cancelled);
        }
        let t = Time::from_frames(index, fps);
        let mut canvas = match top_video_clip(project, t) {
            Some((clip, asset)) => {
                let reader = match video_readers.entry(clip.id) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(e) => e.insert(VideoReader::open(
                        &asset.path,
                        asset.kind() == MediaKind::Image,
                    )?),
                };
                let orientation = reader.orientation();
                let pixel_aspect = reader.pixel_aspect();
                match reader.frame_at(clip.source_time(t))? {
                    Some(src) => fitter.fit(src, orientation, pixel_aspect)?,
                    None => fitter.black(),
                }
            }
            None => fitter.black(),
        };
        video_readers.retain(|id, _| {
            project
                .tracks
                .iter()
                .flat_map(|tr| &tr.clips)
                .any(|c| c.id == *id && c.end() > t)
        });
        muxer.write_video(&mut canvas, index)?;

        let target = Time::from_frames(index + 1, fps)
            .to_samples(rate)
            .min(total_samples);
        while audio_pos < target {
            let n = (total_samples - audio_pos).min(frame_size);
            if audio_pos + n > target && target < total_samples {
                break;
            }
            samples.clear();
            samples.resize(n as usize * OUTPUT_CHANNELS, 0.0);
            mixer.mix(audio_pos, &mut samples)?;
            muxer.write_audio(&samples, audio_pos)?;
            audio_pos += n;
        }

        progress(ExportProgress {
            frame: index as u64 + 1,
            total_frames: total_frames as u64,
        });
    }
    muxer.finish()
}
