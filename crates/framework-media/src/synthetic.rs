//! Synthetic media generators, used by tests and by `framework-cli self-test`.

use crate::MediaError;
use crate::export::Muxer;
use ffmpeg_next::{Dictionary, Packet, codec, encoder, format, frame};
use framework_core::{ExportProfile, Rational};
use std::io::Write;
use std::path::Path;

/// Luma value used for frame `index` of a generated video, so tests can tell frames apart.
pub fn frame_luma(index: i64) -> u8 {
    (32 + (index * 8) % 192) as u8
}

/// Writes an H.264 MP4 whose frame `i` is a flat grey of luma [`frame_luma`]`(i)`,
/// optionally with a 440 Hz stereo sine tone.
pub fn write_test_video(
    path: &Path,
    (width, height): (u32, u32),
    fps: u32,
    frames: i64,
    with_audio: bool,
) -> Result<(), MediaError> {
    let mut profile = ExportProfile::builtin_by_id("draft-540p").expect("built-in profile");
    profile.video.width = width;
    profile.video.height = height;
    profile.video.frame_rate = Rational::new(fps, 1);
    profile.video.preset = "ultrafast".into();
    let rate = profile.audio.sample_rate;
    let mut muxer = Muxer::create(path, &profile, with_audio)?;
    let chunk = muxer.audio_frame_size() as i64;
    let total_samples = frames * i64::from(rate) / i64::from(fps);
    let mut audio_pos = 0i64;
    for i in 0..frames {
        let mut f = frame::Video::new(format::Pixel::YUV420P, width, height);
        f.data_mut(0).fill(frame_luma(i));
        f.data_mut(1).fill(128);
        f.data_mut(2).fill(128);
        muxer.write_video(&mut f, i)?;
        let target = ((i + 1) * i64::from(rate) / i64::from(fps)).min(total_samples);
        while with_audio && audio_pos + chunk <= target {
            let samples: Vec<f32> = (audio_pos..audio_pos + chunk)
                .flat_map(|s| {
                    let v = (s as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5;
                    [v, v]
                })
                .collect();
            muxer.write_audio(&samples, audio_pos)?;
            audio_pos += chunk;
        }
    }
    muxer.finish()
}

/// Writes a single-colour PNG image.
pub fn write_test_png(
    path: &Path,
    (width, height): (u32, u32),
    rgb: [u8; 3],
) -> Result<(), MediaError> {
    crate::init();
    let mut output = format::output_as(path, "image2").map_err(|source| MediaError::Open {
        path: path.to_owned(),
        source,
    })?;
    let png = encoder::find(codec::Id::PNG).ok_or(MediaError::MissingEncoder("png"))?;
    let mut enc = codec::context::Context::new_with_codec(png)
        .encoder()
        .video()?;
    enc.set_width(width);
    enc.set_height(height);
    enc.set_format(format::Pixel::RGB24);
    enc.set_time_base((1, 1));
    let mut enc = enc.open_with(Dictionary::new())?;
    let mut st = output.add_stream(png)?;
    st.set_parameters(&enc);
    output.write_header()?;
    let mut f = frame::Video::new(format::Pixel::RGB24, width, height);
    let stride = f.stride(0);
    for row in f.data_mut(0).chunks_mut(stride) {
        for px in row[..width as usize * 3].as_chunks_mut::<3>().0 {
            px.copy_from_slice(&rgb);
        }
    }
    f.set_pts(Some(0));
    enc.send_frame(&f)?;
    enc.send_eof()?;
    let mut packet = Packet::empty();
    while enc.receive_packet(&mut packet).is_ok() {
        packet.set_stream(0);
        packet.write(&mut output)?;
    }
    output.write_trailer()?;
    Ok(())
}

/// Writes a 16-bit PCM WAV file with a constant-amplitude square wave per channel.
pub fn write_test_wav(
    path: &Path,
    rate: u32,
    channels: u16,
    seconds: f64,
    amplitude: i16,
) -> Result<(), MediaError> {
    let frames = (f64::from(rate) * seconds) as u32;
    let data_len = frames * u32::from(channels) * 2;
    let mut f = std::fs::File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * u32::from(channels) * 2).to_le_bytes())?;
    f.write_all(&(channels * 2).to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    let mut buf = Vec::with_capacity(data_len as usize);
    for i in 0..frames {
        let v = if (i / 50) % 2 == 0 {
            amplitude
        } else {
            -amplitude
        };
        for _ in 0..channels {
            buf.extend_from_slice(&v.to_le_bytes());
        }
    }
    f.write_all(&buf)?;
    Ok(())
}
