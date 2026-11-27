use clap::Parser;
use framework_cli::{Cli, resolve_profile, run};
use std::path::PathBuf;

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("framework-cli-tests-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn exec(args: &[&str]) -> anyhow::Result<String> {
    let mut out = Vec::new();
    let mut argv = vec!["framework-cli"];
    argv.extend_from_slice(args);
    run(Cli::try_parse_from(argv)?, &mut out)?;
    Ok(String::from_utf8(out).unwrap())
}

#[test]
fn profiles_lists_builtins() {
    let out = exec(&["profiles"]).unwrap();
    assert!(out.contains("1080p30"));
    assert!(out.contains("vertical-1080p30"));
    assert_eq!(
        out.lines().count(),
        framework_core::ExportProfile::builtin().len()
    );
}

#[test]
fn resolve_profile_overrides_and_validation() {
    let p = resolve_profile("720p30", Some(640), Some(360), Some(25), Some(1500)).unwrap();
    assert_eq!((p.video.width, p.video.height), (640, 360));
    assert_eq!(p.video.frame_rate.num, 25);
    assert_eq!(p.video.bitrate_kbps, 1500);
    assert!(resolve_profile("nope", None, None, None, None).is_err());
    assert!(resolve_profile("720p30", Some(641), None, None, None).is_err());
    assert!(resolve_profile("720p30", None, None, Some(0), None).is_err());
    assert!(resolve_profile("720p30", None, None, None, Some(0)).is_err());
}

#[test]
fn new_info_probe_export_round_trip() {
    let d = dir();
    let video = d.join("in.mp4");
    let image = d.join("in.png");
    framework_media::synthetic::write_test_video(&video, (64, 36), 10, 10, true).unwrap();
    framework_media::synthetic::write_test_png(&image, (16, 16), [1, 2, 3]).unwrap();
    let project = d.join("p.framework.json");
    let p = project.to_str().unwrap();

    let probe = exec(&["probe", video.to_str().unwrap()]).unwrap();
    assert!(probe.contains("\"kind\": \"video\""));

    exec(&[
        "new",
        p,
        video.to_str().unwrap(),
        image.to_str().unwrap(),
        "--name",
        "Demo",
    ])
    .unwrap();
    let info = exec(&["info", p]).unwrap();
    assert!(info.starts_with("Demo: 00:00:06."), "{info}");
    assert!(info.contains("V1 (Video): 2 clip(s)"));

    // Default name comes from the file stem.
    let unnamed = d.join("unnamed.framework.json");
    exec(&["new", unnamed.to_str().unwrap()]).unwrap();
    assert!(
        exec(&["info", unnamed.to_str().unwrap()])
            .unwrap()
            .starts_with("unnamed.framework:")
    );

    let out = d.join("out.mp4");
    let msg = exec(&[
        "export",
        p,
        "-o",
        out.to_str().unwrap(),
        "-p",
        "draft-540p",
        "--width",
        "64",
        "--height",
        "64",
        "--fps",
        "10",
        "--bitrate",
        "300",
    ])
    .unwrap();
    assert!(msg.contains("wrote"));
    let info = framework_media::probe(&out).unwrap();
    assert_eq!(info.video.unwrap().width, 64);

    assert!(exec(&["export", p, "-o", out.to_str().unwrap(), "-p", "bogus"]).is_err());
    assert!(exec(&["probe", "/nope.mp4"]).is_err());
    assert!(exec(&["new", d.join("x.json").to_str().unwrap(), "/nope.mp4"]).is_err());
    assert!(exec(&["info", "/nope.json"]).is_err());
}

#[test]
fn self_test_passes() {
    let out = exec(&["self-test", dir().join("self").to_str().unwrap()]).unwrap();
    assert!(out.contains("self-test OK"), "{out}");
}
