use framework_core::{Clip, ExportProfile, MediaAsset, MediaKind, Project, Time, TrackKind};
use framework_media::{
    AudioReader, ExportProgress, MediaError, PreviewRenderer, VideoReader, export,
    ffmpeg_version_info, probe, synthetic,
};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

struct Fixtures {
    dir: PathBuf,
    video: PathBuf,
    silent_video: PathBuf,
    long_video: PathBuf,
    image: PathBuf,
    wav: PathBuf,
}

fn fixtures() -> &'static Fixtures {
    static F: OnceLock<Fixtures> = OnceLock::new();
    F.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("framework-media-tests-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = Fixtures {
            video: dir.join("video.mp4"),
            silent_video: dir.join("silent.mp4"),
            long_video: dir.join("long.mp4"),
            image: dir.join("image.png"),
            wav: dir.join("tone.wav"),
            dir,
        };
        synthetic::write_test_video(&f.video, (64, 36), 10, 10, true).unwrap();
        synthetic::write_test_video(&f.silent_video, (48, 48), 10, 10, false).unwrap();
        synthetic::write_test_video(&f.long_video, (32, 18), 10, 60, true).unwrap();
        synthetic::write_test_png(&f.image, (40, 20), [10, 200, 30]).unwrap();
        synthetic::write_test_wav(&f.wav, 44_100, 1, 3.0, 16_000).unwrap();
        f
    })
}

fn tmp(name: &str) -> PathBuf {
    fixtures().dir.join(name)
}

fn luma(reader: &mut VideoReader, t: f64) -> u8 {
    let frame = reader.frame_at(Time::from_seconds(t)).unwrap().unwrap();
    let w = frame.width() as usize;
    let h = frame.height() as usize;
    frame.data(0)[(h / 2) * frame.stride(0) + w / 2]
}

fn close(a: u8, b: u8) -> bool {
    a.abs_diff(b) <= 4
}

#[test]
fn version_info_reports_gpl() {
    let info = ffmpeg_version_info();
    assert!(info.starts_with("libavcodec "), "{info}");
    assert!(info.contains("GPL"), "{info}");
}

#[test]
fn probe_classifies_media() {
    let f = fixtures();
    let v = probe(&f.video).unwrap();
    assert_eq!(v.kind, MediaKind::Video);
    let vi = v.video.as_ref().unwrap();
    assert_eq!((vi.width, vi.height), (64, 36));
    assert_eq!(vi.frame_rate.as_f64(), 10.0);
    assert_eq!(vi.codec, "h264");
    assert_eq!(v.audio.as_ref().unwrap().channels, 2);
    let d = v.duration.unwrap().as_seconds();
    assert!((0.9..1.2).contains(&d), "{d}");

    let s = probe(&f.silent_video).unwrap();
    assert_eq!(s.kind, MediaKind::Video);
    assert!(s.audio.is_none());

    let i = probe(&f.image).unwrap();
    assert_eq!(i.kind, MediaKind::Image);
    assert!(i.duration.is_none());
    assert_eq!(i.video.unwrap().width, 40);

    let a = probe(&f.wav).unwrap();
    assert_eq!(a.kind, MediaKind::Audio);
    assert_eq!(a.audio.as_ref().unwrap().sample_rate, 44_100);
    assert_eq!(a.audio.unwrap().channels, 1);
}

#[test]
fn probe_errors() {
    let missing = probe(Path::new("/definitely/not/here.mp4")).unwrap_err();
    assert!(matches!(missing, MediaError::Open { .. }));
    assert!(missing.to_string().contains("here.mp4"));
    let text = tmp("notes.txt");
    std::fs::write(&text, "not media at all").unwrap();
    assert!(probe(&text).is_err());
}

#[test]
fn video_reader_sequential_and_seeking() {
    let f = fixtures();
    let mut r = VideoReader::open(&f.long_video, false).unwrap();
    assert_eq!((r.width(), r.height()), (32, 18));
    for i in 0..5 {
        assert!(close(
            luma(&mut r, i as f64 * 0.1),
            synthetic::frame_luma(i)
        ));
    }
    // Jump far ahead (seek), then back (seek backwards).
    assert!(close(luma(&mut r, 4.5), synthetic::frame_luma(45)));
    assert!(close(luma(&mut r, 1.0), synthetic::frame_luma(10)));
    // Past the end returns the last frame.
    let end = luma(&mut r, 100.0);
    assert!(close(end, synthetic::frame_luma(59)), "end luma {end}");

    // First request far into the file seeks immediately.
    let mut r = VideoReader::open(&f.long_video, false).unwrap();
    assert!(close(luma(&mut r, 3.0), synthetic::frame_luma(30)));
}

#[test]
fn video_reader_still_and_errors() {
    let f = fixtures();
    let mut r = VideoReader::open(&f.image, true).unwrap();
    let a = r.frame_at(Time::ZERO).unwrap().unwrap().width();
    let b = r
        .frame_at(Time::from_seconds(99.0))
        .unwrap()
        .unwrap()
        .width();
    assert_eq!((a, b), (40, 40));
    assert!(matches!(
        VideoReader::open(&f.wav, false),
        Err(MediaError::NoVideo(_))
    ));
    assert!(matches!(
        VideoReader::open(Path::new("/nope.mp4"), false),
        Err(MediaError::Open { .. })
    ));
}

#[test]
fn audio_reader_mixes_and_seeks() {
    let f = fixtures();
    let mut r = AudioReader::open(&f.wav, 48_000).unwrap();
    let mut buf = vec![0.0f32; 2048];
    r.mix_into(0, &mut buf, 1.0).unwrap();
    let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!((0.4..0.6).contains(&peak), "{peak}");
    // Mono is duplicated to both channels.
    assert!((buf[1000] - buf[1001]).abs() < 1e-6);

    // Gain scales, and mixing adds on top of existing content.
    let mut half = vec![0.0f32; 2048];
    r.mix_into(1024, &mut half, 0.5).unwrap();
    let peak_half = half.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!((0.2..0.3).contains(&peak_half), "{peak_half}");

    // Far ahead (seek), backwards (seek) and past the end (left untouched).
    let mut buf = vec![0.0f32; 512];
    r.mix_into(48_000 * 2, &mut buf, 1.0).unwrap();
    assert!(buf.iter().any(|s| *s != 0.0));
    let mut buf = vec![0.0f32; 512];
    r.mix_into(100, &mut buf, 1.0).unwrap();
    assert!(buf.iter().any(|s| *s != 0.0));
    let mut buf = vec![0.25f32; 512];
    r.mix_into(48_000 * 10, &mut buf, 1.0).unwrap();
    assert!(buf.iter().all(|s| *s == 0.25));

    // First request far into the file.
    let mut r = AudioReader::open(&f.long_video, 48_000).unwrap();
    let mut buf = vec![0.0f32; 512];
    r.mix_into(48_000 * 4, &mut buf, 1.0).unwrap();
    assert!(buf.iter().any(|s| *s != 0.0));

    assert!(matches!(
        AudioReader::open(&f.silent_video, 48_000),
        Err(MediaError::NoAudio(_))
    ));
    assert!(matches!(
        AudioReader::open(Path::new("/nope.wav"), 48_000),
        Err(MediaError::Open { .. })
    ));
}

fn asset(project: &mut Project, path: &Path) -> framework_core::MediaId {
    project.add_media(MediaAsset::new(path, probe(path).unwrap()))
}

fn tiny_profile() -> ExportProfile {
    let mut p = ExportProfile::builtin_by_id("draft-540p").unwrap();
    p.video.width = 64;
    p.video.height = 64;
    p.video.preset = "ultrafast".into();
    p
}

/// V1: video 0–1 s, V2: image 0.5–1.5 s (on top), A1: wav 0–3 s,
/// plus a hidden video track and a muted audio track that must be ignored.
fn layered_project() -> Project {
    let f = fixtures();
    let mut p = Project::new("layered");
    let video = asset(&mut p, &f.video);
    let image = asset(&mut p, &f.image);
    let wav = asset(&mut p, &f.wav);
    let silent = asset(&mut p, &f.silent_video);
    let v1 = p.default_track_for(MediaKind::Video);
    p.append_media(v1, video).unwrap();
    let v2 = p.add_track(TrackKind::Video);
    p.add_clip(
        v2,
        Clip::new(
            image,
            Time::from_seconds(0.5),
            Time::ZERO,
            Time::from_seconds(1.0),
        ),
    )
    .unwrap();
    let a1 = p.default_track_for(MediaKind::Audio);
    p.append_media(a1, wav).unwrap();
    let hidden = p.add_track(TrackKind::Video);
    p.append_media(hidden, silent).unwrap();
    p.tracks.iter_mut().find(|t| t.id == hidden).unwrap().hidden = true;
    let muted = p.add_track(TrackKind::Audio);
    p.append_media(muted, wav).unwrap();
    p.tracks.iter_mut().find(|t| t.id == muted).unwrap().muted = true;
    p
}

#[test]
fn export_layered_project() {
    let project = layered_project();
    let out = tmp("layered.mp4");
    let mut last = ExportProgress {
        frame: 0,
        total_frames: 0,
    };
    export(
        &project,
        &tiny_profile(),
        &out,
        &AtomicBool::new(false),
        |p| last = p,
    )
    .unwrap();
    assert_eq!(last.frame, last.total_frames);
    assert_eq!(last.total_frames, 90);
    assert_eq!(last.fraction(), 1.0);

    let info = probe(&out).unwrap();
    let d = info.duration.unwrap().as_seconds();
    assert!((2.9..3.2).contains(&d), "{d}");
    assert_eq!(info.video.as_ref().unwrap().width, 64);
    assert_eq!(info.audio.as_ref().unwrap().sample_rate, 48_000);

    // 0.2 s: grey video; 1.2 s: green image on top (letterboxed, so the
    // centre is green and the top row black); 2.5 s: nothing -> black.
    let mut r = VideoReader::open(&out, false).unwrap();
    assert!(close(luma(&mut r, 0.2), synthetic::frame_luma(2)));
    let image_frame = r.frame_at(Time::from_seconds(1.2)).unwrap().unwrap();
    let centre = image_frame.data(0)[32 * image_frame.stride(0) + 32];
    let top = image_frame.data(0)[2];
    assert!(centre > 100, "green luma {centre}");
    assert!(top <= 20, "letterbox {top}");
    assert!(luma(&mut r, 2.5) <= 20);

    // Audio is present throughout (wav lasts 3 s).
    let mut a = AudioReader::open(&out, 48_000).unwrap();
    let mut buf = vec![0.0f32; 4096];
    a.mix_into(48_000 * 2, &mut buf, 1.0).unwrap();
    assert!(buf.iter().any(|s| s.abs() > 0.1));
}

#[test]
fn export_errors_and_cancellation() {
    let profile = tiny_profile();
    let empty = Project::new("empty");
    let out = tmp("never.mp4");
    let err = export(&empty, &profile, &out, &AtomicBool::new(false), |_| {}).unwrap_err();
    assert!(matches!(err, MediaError::EmptyTimeline));

    let project = layered_project();
    let out = tmp("cancelled.mp4");
    let err = export(&project, &profile, &out, &AtomicBool::new(true), |_| {}).unwrap_err();
    assert!(matches!(err, MediaError::Cancelled));
    assert!(!out.exists());

    let bad = Path::new("/no/such/dir/out.mp4");
    let err = export(&project, &profile, bad, &AtomicBool::new(false), |_| {}).unwrap_err();
    assert!(
        matches!(err, MediaError::Open { .. } | MediaError::Ffmpeg(_)),
        "{err}"
    );

    // A clip whose media file disappeared fails cleanly.
    let gone = tmp("gone.mp4");
    std::fs::copy(&fixtures().video, &gone).unwrap();
    let mut p = Project::new("gone");
    let id = asset(&mut p, &gone);
    let v1 = p.default_track_for(MediaKind::Video);
    p.append_media(v1, id).unwrap();
    std::fs::remove_file(&gone).unwrap();
    let out = tmp("gone-out.mp4");
    assert!(export(&p, &profile, &out, &AtomicBool::new(false), |_| {}).is_err());
    assert!(!out.exists());

    assert_eq!(
        ExportProgress {
            frame: 0,
            total_frames: 0
        }
        .fraction(),
        1.0
    );
}

#[test]
fn preview_renders_layers_and_gaps() {
    let project = layered_project();
    let mut r = PreviewRenderer::new(33, 21);
    assert_eq!(r.size(), (32, 20));
    let px = |img: &framework_media::RgbaImage, x: u32, y: u32| {
        let i = ((y * img.width + x) * 4) as usize;
        [
            img.pixels[i],
            img.pixels[i + 1],
            img.pixels[i + 2],
            img.pixels[i + 3],
        ]
    };
    let grey = r.render(&project, Time::from_seconds(0.2)).unwrap();
    assert_eq!(grey.pixels.len(), 32 * 20 * 4);
    // Frame 2 has luma 48 (limited range) -> roughly RGB 37.
    let [g, _, _, alpha] = px(&grey, 16, 10);
    assert!((25..50).contains(&g) && alpha == 255, "grey {g}");
    let green = r.render(&project, Time::from_seconds(1.2)).unwrap();
    let [rr, gg, bb, _] = px(&green, 16, 10);
    assert!(gg > 150 && rr < 60 && bb < 80, "{rr} {gg} {bb}");
    let black = r.render(&project, Time::from_seconds(2.5)).unwrap();
    assert_eq!(px(&black, 16, 10), [0, 0, 0, 255]);
    // Rendering an earlier time again exercises backwards access.
    assert!(r.render(&project, Time::ZERO).is_ok());
    // Missing media surfaces an error.
    let mut broken = project.clone();
    broken.media[0].path = PathBuf::from("/nope.mp4");
    let mut r2 = PreviewRenderer::new(16, 16);
    assert!(r2.render(&broken, Time::from_seconds(0.1)).is_err());
}

#[test]
fn synthetic_wav_error_path() {
    assert!(synthetic::write_test_wav(Path::new("/no/such/dir/x.wav"), 8000, 1, 0.1, 1).is_err());
    assert!(synthetic::write_test_png(Path::new("/no/such/dir/x.png"), (2, 2), [0, 0, 0]).is_err());
    assert!(
        synthetic::write_test_video(Path::new("/no/such/dir/x.mp4"), (16, 16), 10, 1, false)
            .is_err()
    );
}
