//! Core data model for Framework: time, projects, timelines and export profiles.
//!
//! This crate is deliberately free of FFmpeg and GUI dependencies so it can be
//! unit-tested quickly and shared by every frontend.

pub mod profile;
pub mod project;
pub mod time;

pub use profile::{AudioCodec, AudioSettings, Container, ExportProfile, VideoCodec, VideoSettings};
pub use project::{
    AudioStreamInfo, Clip, ClipId, MediaAsset, MediaId, MediaInfo, MediaKind, Project,
    ProjectError, ProjectSettings, Track, TrackId, TrackKind, VideoStreamInfo,
};
pub use time::{Rational, Time};
