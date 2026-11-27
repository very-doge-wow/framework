//! Implementation of the `framework-cli` command-line tool.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use framework_core::{ExportProfile, MediaAsset, Project, Rational, SaveMode};
use std::ffi::OsStr;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

#[derive(Debug, Parser)]
#[command(
    name = "framework-cli",
    version,
    about = "Framework video editor command-line tool"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List the built-in export profiles.
    Profiles,
    /// Print technical information about a media file as JSON.
    Probe { file: PathBuf },
    /// Create a project, appending each media file to the timeline in order.
    New {
        project: PathBuf,
        media: Vec<PathBuf>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Summarise a project file.
    Info { project: PathBuf },
    /// Copy a project and embed all of its media into the new `.fwk` file.
    Collect { project: PathBuf, output: PathBuf },
    /// Render a project to an MP4 file.
    Export {
        project: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Built-in profile id (see `profiles`).
        #[arg(short, long, default_value = "1080p30")]
        profile: String,
        #[arg(long)]
        width: Option<u32>,
        #[arg(long)]
        height: Option<u32>,
        #[arg(long)]
        fps: Option<u32>,
        /// Video bitrate in kbit/s.
        #[arg(long)]
        bitrate: Option<u32>,
    },
    /// Generate synthetic media in DIR, build a project from it and export it.
    /// Used to verify that a packaged build and its bundled FFmpeg work.
    SelfTest { dir: PathBuf },
}

/// Resolves a built-in profile and applies command-line overrides.
pub fn resolve_profile(
    id: &str,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<u32>,
    bitrate: Option<u32>,
) -> Result<ExportProfile> {
    let mut profile = ExportProfile::builtin_by_id(id)
        .with_context(|| format!("unknown profile '{id}' (run `framework-cli profiles`)"))?;
    if let Some(w) = width {
        profile.video.width = w;
    }
    if let Some(h) = height {
        profile.video.height = h;
    }
    if let Some(f) = fps {
        profile.video.frame_rate = Rational::new(f, 1);
    }
    if let Some(b) = bitrate {
        profile.video.bitrate_kbps = b;
    }
    if profile.video.width % 2 != 0 || profile.video.height % 2 != 0 {
        bail!("width and height must be even");
    }
    if profile.video.frame_rate.num == 0 || profile.video.bitrate_kbps == 0 {
        bail!("frame rate and bitrate must be positive");
    }
    Ok(profile)
}

pub fn run(cli: Cli, out: &mut impl Write) -> Result<()> {
    match cli.command {
        Command::Profiles => {
            for p in ExportProfile::builtin() {
                writeln!(
                    out,
                    "{:<18} {}x{} @ {} fps, {} kbit/s video, {} kbit/s audio  ({})",
                    p.id,
                    p.video.width,
                    p.video.height,
                    p.video.frame_rate,
                    p.video.bitrate_kbps,
                    p.audio.bitrate_kbps,
                    p.name
                )?;
            }
        }
        Command::Probe { file } => {
            let info = framework_media::probe(&file)?;
            writeln!(out, "{}", serde_json::to_string_pretty(&info)?)?;
        }
        Command::New {
            project: path,
            media,
            name,
        } => {
            let name = name.unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Untitled".into())
            });
            let mut project = Project::new(name);
            for file in media {
                let file = std::fs::canonicalize(&file)
                    .with_context(|| format!("cannot open {}", file.display()))?;
                let info = framework_media::probe(&file)?;
                let kind = info.kind;
                let id = project.add_media(MediaAsset::new(file, info));
                let track = project.default_track_for(kind);
                project.append_media(track, id)?;
            }
            project.save(&path)?;
            writeln!(out, "wrote {}", path.display())?;
        }
        Command::Info { project } => {
            let project = Project::load(&project)?;
            writeln!(out, "{}: {}", project.name, project.duration())?;
            for track in &project.tracks {
                writeln!(
                    out,
                    "  {} ({:?}): {} clip(s)",
                    track.name,
                    track.kind,
                    track.clips.len()
                )?;
            }
        }
        Command::Collect { project, output } => {
            let mut project = Project::load(&project)?;
            project.save_as(&output, SaveMode::Collect)?;
            writeln!(out, "wrote {} with embedded media", output.display())?;
        }
        Command::Export {
            project,
            output,
            profile,
            width,
            height,
            fps,
            bitrate,
        } => {
            let profile = resolve_profile(&profile, width, height, fps, bitrate)?;
            let project = Project::load(&project)?;
            let cancel = AtomicBool::new(false);
            let mut last = -1;
            framework_media::export(&project, &profile, &output, &cancel, |p| {
                let pct = (p.fraction() * 100.0) as i32;
                if pct / 10 != last / 10 {
                    last = pct;
                    eprintln!("{pct:3}% ({}/{} frames)", p.frame, p.total_frames);
                }
            })?;
            writeln!(out, "wrote {}", output.display())?;
        }
        Command::SelfTest { dir } => self_test(&dir, out)?,
    }
    Ok(())
}

fn self_test(dir: &std::path::Path, out: &mut impl Write) -> Result<()> {
    use framework_media::synthetic;
    std::fs::create_dir_all(dir)?;
    let video = dir.join("self-test-video.mp4");
    let image = dir.join("self-test-image.png");
    let audio = dir.join("self-test-audio.wav");
    synthetic::write_test_video(&video, (320, 180), 30, 30, true)?;
    synthetic::write_test_png(&image, (64, 64), [200, 40, 40])?;
    synthetic::write_test_wav(&audio, 44_100, 1, 1.0, 8000)?;
    let project = dir.join("self-test.fwk");
    let collected = dir.join("self-test-collected.fwk");
    let output = dir.join("self-test-export.mp4");
    run(
        Cli::parse_from([
            OsStr::new("framework-cli"),
            OsStr::new("new"),
            project.as_os_str(),
            video.as_os_str(),
            image.as_os_str(),
            audio.as_os_str(),
        ]),
        out,
    )?;
    run(
        Cli::parse_from([
            OsStr::new("framework-cli"),
            OsStr::new("export"),
            project.as_os_str(),
            OsStr::new("-o"),
            output.as_os_str(),
            OsStr::new("-p"),
            OsStr::new("draft-540p"),
        ]),
        out,
    )?;
    check_self_test_export(&output)?;
    // Embedded media must render without the original files.
    let cli = |args: &[&OsStr]| Cli::parse_from([OsStr::new("framework-cli")].iter().chain(args));
    run(
        cli(&[
            OsStr::new("collect"),
            project.as_os_str(),
            collected.as_os_str(),
        ]),
        out,
    )?;
    for file in [&video, &image, &audio] {
        std::fs::remove_file(file)?;
    }
    run(
        cli(&[
            OsStr::new("export"),
            collected.as_os_str(),
            OsStr::new("-o"),
            output.as_os_str(),
            OsStr::new("-p"),
            OsStr::new("draft-540p"),
        ]),
        out,
    )?;
    let duration = check_self_test_export(&output)?;
    writeln!(out, "self-test OK ({duration:.2} s)")?;
    Ok(())
}

fn check_self_test_export(output: &std::path::Path) -> Result<f64> {
    let info = framework_media::probe(output)?;
    let duration = info.duration.unwrap_or_default().as_seconds();
    if info.video.is_none() || info.audio.is_none() || !(5.5..6.5).contains(&duration) {
        bail!("self-test export is invalid: {info:?}");
    }
    Ok(duration)
}
