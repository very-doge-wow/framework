use crate::MediaError;
use ffmpeg_next::{codec, format, frame, media, software::scaling, threading};
use framework_core::Time;
use std::path::Path;

/// Seeking is cheaper than decoding forward beyond this distance.
const SEEK_THRESHOLD: Time = Time::from_flicks(2 * Time::FLICKS_PER_SECOND);

/// Decodes frames from the best video stream of a file, optimised for
/// monotonically increasing requests (playback/export) but able to seek.
pub struct VideoReader {
    input: format::context::Input,
    stream_index: usize,
    decoder: codec::decoder::Video,
    time_base: (i32, i32),
    start_pts: i64,
    still: bool,
    current: Option<(Time, frame::Video)>,
    pending: Option<(Time, frame::Video)>,
    eof: bool,
    started: bool,
    quarter_turns: u32,
}

impl VideoReader {
    /// Opens `path`. Set `still` for images: the single frame is then returned for any time.
    pub fn open(path: &Path, still: bool) -> Result<Self, MediaError> {
        crate::init();
        let input = format::input(path).map_err(|source| MediaError::Open {
            path: path.to_owned(),
            source,
        })?;
        let stream = input
            .streams()
            .best(media::Type::Video)
            .ok_or_else(|| MediaError::NoVideo(path.to_owned()))?;
        let stream_index = stream.index();
        let tb = stream.time_base();
        let start_pts = reference_start(&input, &stream);
        let quarter_turns = display_quarter_turns(&stream);
        let mut ctx = codec::context::Context::from_parameters(stream.parameters())?;
        ctx.set_threading(threading::Config::kind(threading::Type::Frame));
        let decoder = ctx.decoder().video()?;
        Ok(Self {
            input,
            stream_index,
            decoder,
            time_base: (tb.numerator(), tb.denominator()),
            start_pts,
            still,
            current: None,
            pending: None,
            eof: false,
            started: false,
            quarter_turns,
        })
    }

    pub fn width(&self) -> u32 {
        self.decoder.width()
    }

    pub fn height(&self) -> u32 {
        self.decoder.height()
    }

    /// Clockwise quarter turns needed to show the frames upright (display matrix).
    pub fn quarter_turns(&self) -> u32 {
        self.quarter_turns
    }

    fn pts_to_time(&self, pts: i64) -> Time {
        Time::from_timebase(pts - self.start_pts, self.time_base.0, self.time_base.1)
    }

    fn seek(&mut self, t: Time) -> Result<(), MediaError> {
        let start = Time::from_timebase(self.start_pts, self.time_base.0, self.time_base.1);
        let ts = (t + start).to_timebase(1, ffmpeg_next::ffi::AV_TIME_BASE);
        self.input.seek(ts, ..ts)?;
        self.decoder.flush();
        self.current = None;
        self.pending = None;
        self.eof = false;
        Ok(())
    }

    fn decode_next(&mut self) -> Result<Option<(Time, frame::Video)>, MediaError> {
        loop {
            let mut decoded = frame::Video::empty();
            if self.decoder.receive_frame(&mut decoded).is_ok() {
                let pts = decoded.timestamp().unwrap_or(self.start_pts);
                return Ok(Some((self.pts_to_time(pts), decoded)));
            }
            if self.eof {
                return Ok(None);
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

    /// Returns the frame that is visible at `t` (relative to the start of the media).
    pub fn frame_at(&mut self, t: Time) -> Result<Option<&frame::Video>, MediaError> {
        if self.still {
            if self.current.is_none() {
                self.current = self.decode_next()?;
            }
            return Ok(self.current.as_ref().map(|(_, f)| f));
        }

        let behind = self.current.as_ref().is_some_and(|(ct, _)| t < *ct);
        let far_ahead = self
            .pending
            .as_ref()
            .or(self.current.as_ref())
            .is_some_and(|(pt, _)| t > *pt + SEEK_THRESHOLD);
        if behind || far_ahead || (!self.started && t > SEEK_THRESHOLD) {
            self.seek(t)?;
        }
        self.started = true;

        loop {
            if self.pending.is_none() {
                self.pending = self.decode_next()?;
            }
            match &self.pending {
                Some((pt, _)) if *pt <= t || self.current.is_none() => {
                    let first = self.current.is_none() && *pt > t;
                    self.current = self.pending.take();
                    if first {
                        break;
                    }
                }
                _ => break,
            }
        }
        Ok(self.current.as_ref().map(|(_, f)| f))
    }
}

/// Start of the container in `stream`'s time base. Using one origin for all
/// streams keeps the offset between late-starting audio and video.
pub(crate) fn reference_start(
    input: &format::context::Input,
    stream: &format::stream::Stream,
) -> i64 {
    let own = stream.start_time();
    // SAFETY: `input` owns a valid, opened AVFormatContext.
    let container = unsafe { (*input.as_ptr()).start_time };
    let none = ffmpeg_next::ffi::AV_NOPTS_VALUE;
    if container == none {
        return if own == none { 0 } else { own };
    }
    let base = ffmpeg_next::ffi::AVRational {
        num: 1,
        den: ffmpeg_next::ffi::AV_TIME_BASE,
    };
    // SAFETY: pure arithmetic on plain values.
    let container =
        unsafe { ffmpeg_next::ffi::av_rescale_q(container, base, stream.time_base().into()) };
    if own == none {
        container
    } else {
        container.min(own)
    }
}

/// Clockwise quarter turns from the stream's display matrix (phone footage).
pub(crate) fn display_quarter_turns(stream: &format::stream::Stream) -> u32 {
    use ffmpeg_next::ffi;
    // SAFETY: the stream and its codec parameters are valid while `stream` is borrowed;
    // the side data is only read when FFmpeg reports a full 3×3 matrix.
    unsafe {
        let par = (*stream.as_ptr()).codecpar;
        let sd = ffi::av_packet_side_data_get(
            (*par).coded_side_data,
            (*par).nb_coded_side_data,
            ffi::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
        );
        if sd.is_null() || (*sd).size < 9 * std::mem::size_of::<i32>() {
            return 0;
        }
        quarter_turns_from_angle(ffi::av_display_rotation_get((*sd).data.cast::<i32>()))
    }
}

/// Converts FFmpeg's counter-clockwise display rotation into clockwise quarter turns.
pub(crate) fn quarter_turns_from_angle(ccw_degrees: f64) -> u32 {
    if !ccw_degrees.is_finite() {
        return 0;
    }
    ((-ccw_degrees / 90.0).round() as i64).rem_euclid(4) as u32
}

/// Rotates a picture clockwise by `turns` quarter turns (dimensions must be even for YUV).
fn rotate(src: &frame::Video, turns: u32) -> frame::Video {
    let (w, h) = (src.width(), src.height());
    let (dw, dh) = if turns % 2 == 1 { (h, w) } else { (w, h) };
    let format = src.format();
    let mut dst = frame::Video::new(format, dw, dh);
    let bpp = bytes_per_pixel(format);
    for plane in 0..src.planes() {
        let (sx, sy) = plane_scale(format, plane);
        let (pw, ph) = ((w / sx) as usize, (h / sy) as usize);
        let (ss, ds) = (src.stride(plane), dst.stride(plane));
        let s = src.data(plane);
        let d = dst.data_mut(plane);
        for y in 0..ph {
            for x in 0..pw {
                let (dx, dy) = match turns % 4 {
                    1 => (ph - 1 - y, x),
                    2 => (pw - 1 - x, ph - 1 - y),
                    3 => (y, pw - 1 - x),
                    _ => (x, y),
                };
                let si = y * ss + x * bpp;
                let di = dy * ds + dx * bpp;
                d[di..di + bpp].copy_from_slice(&s[si..si + bpp]);
            }
        }
    }
    dst
}

/// Source format, width, height and quarter turns a cached scaler was built for.
type ScalerKey = (format::Pixel, u32, u32, u32);

/// Fits frames of arbitrary size and format into a fixed-size canvas,
/// preserving aspect ratio (letterboxing / pillarboxing).
pub(crate) struct Fitter {
    format: format::Pixel,
    width: u32,
    height: u32,
    scaler: Option<(ScalerKey, scaling::Context, frame::Video)>,
}

impl Fitter {
    pub(crate) fn new(format: format::Pixel, width: u32, height: u32) -> Self {
        Self {
            format,
            width,
            height,
            scaler: None,
        }
    }

    /// Size and even-aligned offset of a `src_w`×`src_h` picture fitted into the canvas.
    pub(crate) fn fit_rect(&self, src_w: u32, src_h: u32) -> (u32, u32, u32, u32) {
        if src_w == 0 || src_h == 0 {
            return (self.width, self.height, 0, 0);
        }
        let scale = f64::min(
            f64::from(self.width) / f64::from(src_w),
            f64::from(self.height) / f64::from(src_h),
        );
        let even = |v: f64| ((v.round() as u32) & !1).max(2);
        let w = even(f64::from(src_w) * scale).min(self.width);
        let h = even(f64::from(src_h) * scale).min(self.height);
        let x = ((self.width - w) / 2) & !1;
        let y = ((self.height - h) / 2) & !1;
        (w, h, x, y)
    }

    /// A black canvas in the target format.
    pub(crate) fn black(&self) -> frame::Video {
        let mut canvas = frame::Video::new(self.format, self.width, self.height);
        fill_black(&mut canvas);
        canvas
    }

    /// Renders `src`, rotated clockwise by `turns` quarter turns, letterboxed into a fresh canvas.
    pub(crate) fn fit(
        &mut self,
        src: &frame::Video,
        turns: u32,
    ) -> Result<frame::Video, MediaError> {
        let turns = turns % 4;
        let sideways = turns % 2 == 1;
        let (sw, sh) = if sideways {
            (src.height(), src.width())
        } else {
            (src.width(), src.height())
        };
        let (w, h, x, y) = self.fit_rect(sw, sh);
        let (scale_w, scale_h) = if sideways { (h, w) } else { (w, h) };
        let key = (src.format(), src.width(), src.height(), turns);
        if self.scaler.as_ref().is_none_or(|(k, _, _)| *k != key) {
            let ctx = scaling::Context::get(
                src.format(),
                src.width(),
                src.height(),
                self.format,
                scale_w,
                scale_h,
                scaling::Flags::BICUBIC,
            )?;
            self.scaler = Some((key, ctx, frame::Video::empty()));
        }
        let full = w == self.width && h == self.height;
        let mut canvas = if full { None } else { Some(self.black()) };
        let (_, ctx, scaled) = self.scaler.as_mut().expect("scaler initialised above");
        ctx.run(src, scaled)?;
        let rotated = (turns != 0).then(|| rotate(scaled, turns));
        let picture = rotated.as_ref().unwrap_or(scaled);
        match canvas.as_mut() {
            None => Ok(picture.clone()),
            Some(c) => {
                blit(picture, c, x, y);
                Ok(canvas.expect("canvas exists"))
            }
        }
    }
}

fn plane_scale(format: format::Pixel, plane: usize) -> (u32, u32) {
    match (format, plane) {
        (format::Pixel::YUV420P, 1 | 2) => (2, 2),
        _ => (1, 1),
    }
}

fn bytes_per_pixel(format: format::Pixel) -> usize {
    match format {
        format::Pixel::RGBA => 4,
        _ => 1,
    }
}

fn fill_black(canvas: &mut frame::Video) {
    let format = canvas.format();
    for plane in 0..canvas.planes() {
        let value = match (format, plane) {
            (format::Pixel::YUV420P, 0) => 16,
            (format::Pixel::YUV420P, _) => 128,
            _ => 0,
        };
        canvas.data_mut(plane).fill(value);
    }
    if format == format::Pixel::RGBA {
        let stride = canvas.stride(0);
        let width = canvas.width() as usize;
        for row in canvas.data_mut(0).chunks_mut(stride) {
            for px in row[..width * 4].as_chunks_mut::<4>().0 {
                px[3] = 255;
            }
        }
    }
}

fn blit(src: &frame::Video, dst: &mut frame::Video, x: u32, y: u32) {
    let format = dst.format();
    let bpp = bytes_per_pixel(format);
    for plane in 0..dst.planes() {
        let (sx, sy) = plane_scale(format, plane);
        let src_stride = src.stride(plane);
        let dst_stride = dst.stride(plane);
        let row_bytes = (src.width() / sx) as usize * bpp;
        let rows = (src.height() / sy) as usize;
        let off_x = (x / sx) as usize * bpp;
        let off_y = (y / sy) as usize;
        let src_data = src.data(plane);
        let dst_data = dst.data_mut(plane);
        for row in 0..rows {
            let s = row * src_stride;
            let d = (row + off_y) * dst_stride + off_x;
            dst_data[d..d + row_bytes].copy_from_slice(&src_data[s..s + row_bytes]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(w: u32, h: u32) -> frame::Video {
        let mut f = frame::Video::new(format::Pixel::RGBA, w, h);
        let stride = f.stride(0);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = y * stride + x * 4;
                f.data_mut(0)[i..i + 4].copy_from_slice(&[
                    (y * w as usize + x) as u8 + 1,
                    0,
                    0,
                    255,
                ]);
            }
        }
        f
    }

    fn red(f: &frame::Video, x: usize, y: usize) -> u8 {
        f.data(0)[y * f.stride(0) + x * 4]
    }

    #[test]
    fn display_angles_map_to_clockwise_turns() {
        assert_eq!(quarter_turns_from_angle(0.0), 0);
        assert_eq!(quarter_turns_from_angle(-90.0), 1);
        assert_eq!(quarter_turns_from_angle(90.0), 3);
        assert_eq!(quarter_turns_from_angle(180.0), 2);
        assert_eq!(quarter_turns_from_angle(-180.0), 2);
        assert_eq!(quarter_turns_from_angle(f64::NAN), 0);
    }

    #[test]
    fn rotate_moves_pixels_clockwise() {
        // 3×2 source:  1 2 3
        //              4 5 6
        let src = rgba(3, 2);
        let r1 = rotate(&src, 1);
        assert_eq!((r1.width(), r1.height()), (2, 3));
        assert_eq!([red(&r1, 0, 0), red(&r1, 1, 0), red(&r1, 0, 2)], [4, 1, 6]);
        let r2 = rotate(&src, 2);
        assert_eq!((r2.width(), r2.height()), (3, 2));
        assert_eq!([red(&r2, 0, 0), red(&r2, 2, 1)], [6, 1]);
        let r3 = rotate(&src, 3);
        assert_eq!((r3.width(), r3.height()), (2, 3));
        assert_eq!([red(&r3, 0, 0), red(&r3, 1, 0), red(&r3, 0, 2)], [3, 6, 1]);

        let mut yuv = frame::Video::new(format::Pixel::YUV420P, 4, 2);
        yuv.data_mut(0).fill(50);
        yuv.data_mut(1).fill(60);
        yuv.data_mut(2).fill(70);
        let ry = rotate(&yuv, 1);
        assert_eq!((ry.width(), ry.height()), (2, 4));
        assert_eq!((ry.data(0)[0], ry.data(1)[0], ry.data(2)[0]), (50, 60, 70));
    }

    #[test]
    fn fit_applies_rotation_before_letterboxing() {
        let mut fitter = Fitter::new(format::Pixel::RGBA, 8, 8);
        // A wide 4×2 picture turned sideways becomes tall: pillarboxed at x = 2..6.
        let out = fitter.fit(&rgba(4, 2), 1).unwrap();
        assert_eq!((out.width(), out.height()), (8, 8));
        assert_eq!(red(&out, 0, 4), 0);
        assert_ne!(red(&out, 4, 4), 0);
        // Unrotated, the same picture is letterboxed instead.
        let out = fitter.fit(&rgba(4, 2), 0).unwrap();
        assert_eq!(red(&out, 4, 0), 0);
        assert_ne!(red(&out, 4, 4), 0);
    }

    #[test]
    fn plain_files_need_no_rotation_or_offset() {
        let dir = std::env::temp_dir().join(format!("framework-video-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plain.mp4");
        crate::synthetic::write_test_video(&path, (32, 18), 10, 5, true).unwrap();
        let reader = VideoReader::open(&path, false).unwrap();
        assert_eq!(reader.quarter_turns(), 0);
        let input = format::input(&path).unwrap();
        for stream in input.streams() {
            assert!(reference_start(&input, &stream) <= stream.start_time().max(0));
        }
    }
}
