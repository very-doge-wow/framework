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
        let start_pts = if stream.start_time() == ffmpeg_next::ffi::AV_NOPTS_VALUE {
            0
        } else {
            stream.start_time()
        };
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
        })
    }

    pub fn width(&self) -> u32 {
        self.decoder.width()
    }

    pub fn height(&self) -> u32 {
        self.decoder.height()
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

/// Fits frames of arbitrary size and format into a fixed-size canvas,
/// preserving aspect ratio (letterboxing / pillarboxing).
pub(crate) struct Fitter {
    format: format::Pixel,
    width: u32,
    height: u32,
    scaler: Option<((format::Pixel, u32, u32), scaling::Context, frame::Video)>,
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

    /// Renders `src` letterboxed into a fresh canvas.
    pub(crate) fn fit(&mut self, src: &frame::Video) -> Result<frame::Video, MediaError> {
        let (w, h, x, y) = self.fit_rect(src.width(), src.height());
        let key = (src.format(), src.width(), src.height());
        if self.scaler.as_ref().is_none_or(|(k, _, _)| *k != key) {
            let ctx = scaling::Context::get(
                src.format(),
                src.width(),
                src.height(),
                self.format,
                w,
                h,
                scaling::Flags::BICUBIC,
            )?;
            self.scaler = Some((key, ctx, frame::Video::empty()));
        }
        let full = w == self.width && h == self.height;
        let mut canvas = if full { None } else { Some(self.black()) };
        let (_, ctx, scaled) = self.scaler.as_mut().expect("scaler initialised above");
        ctx.run(src, scaled)?;
        match canvas.as_mut() {
            None => Ok(scaled.clone()),
            Some(c) => {
                blit(scaled, c, x, y);
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
