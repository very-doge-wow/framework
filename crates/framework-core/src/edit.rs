//! Timeline editing operations. Each operation either succeeds completely or
//! leaves the project unchanged.
use crate::{
    Clip, ClipId, GroupId, MediaId, MediaKind, Project, ProjectError, Time, Track, TrackId,
    TrackKind,
};
use std::collections::{HashMap, HashSet};

/// `delta`, or zero when the bounds would push an edge against the drag
/// direction (e.g. a group member already shorter than the minimum).
fn towards(wanted: Time, delta: Time) -> Time {
    if delta.flicks().signum() == wanted.flicks().signum() {
        delta
    } else {
        Time::ZERO
    }
}

/// Far beyond any real timeline, used as "no limit".
const UNLIMITED: Time = Time::from_flicks(i64::MAX / 4);

impl Project {
    /// Track and clip index of a clip.
    pub fn locate(&self, id: ClipId) -> Option<(usize, usize)> {
        self.tracks
            .iter()
            .enumerate()
            .find_map(|(t, track)| track.clips.iter().position(|c| c.id == id).map(|c| (t, c)))
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.locate(id).map(|(t, c)| &self.tracks[t].clips[c])
    }

    fn clip_mut(&mut self, id: ClipId) -> Result<&mut Clip, ProjectError> {
        let (t, c) = self.locate(id).ok_or(ProjectError::UnknownClip(id))?;
        Ok(&mut self.tracks[t].clips[c])
    }

    /// The existing clips of `ids` plus every clip grouped with one of them.
    pub fn with_groups(&self, ids: &[ClipId]) -> Vec<ClipId> {
        let mut out: Vec<ClipId> = Vec::new();
        for id in ids {
            if self.clip(*id).is_some() && !out.contains(id) {
                out.push(*id);
            }
        }
        let groups: HashSet<GroupId> = out.iter().filter_map(|id| self.clip(*id)?.group).collect();
        for clip in self.tracks.iter().flat_map(|t| &t.clips) {
            if clip.group.is_some_and(|g| groups.contains(&g)) && !out.contains(&clip.id) {
                out.push(clip.id);
            }
        }
        out
    }

    /// Groups the clips (merging existing groups they belong to).
    pub fn group_clips(&mut self, ids: &[ClipId]) -> Result<GroupId, ProjectError> {
        let ids = self.with_groups(ids);
        if ids.len() < 2 {
            return Err(ProjectError::InvalidEdit(
                "select at least two clips to group",
            ));
        }
        let group = GroupId::new();
        for id in ids {
            self.clip_mut(id)?.group = Some(group);
        }
        Ok(group)
    }

    /// Removes the groups of the clips; returns how many clips were ungrouped.
    pub fn ungroup_clips(&mut self, ids: &[ClipId]) -> usize {
        let ids = self.with_groups(ids);
        let mut n = 0;
        for id in ids {
            if let Ok(clip) = self.clip_mut(id)
                && clip.group.take().is_some()
            {
                n += 1;
            }
        }
        n
    }

    /// Length of the clip's source media, `None` for stills (images), which
    /// can be extended indefinitely.
    fn source_length(&self, clip: &Clip) -> Option<Time> {
        let asset = self.media(clip.media)?;
        match asset.kind() {
            MediaKind::Image => None,
            _ => asset.info.duration,
        }
    }

    /// Splits every clip of `ids` that spans `at`. The right-hand parts of a
    /// group form a new group. Returns the ids of the new right-hand clips.
    pub fn split_clips(&mut self, ids: &[ClipId], at: Time) -> Vec<ClipId> {
        let mut groups: HashMap<GroupId, GroupId> = HashMap::new();
        let mut created = Vec::new();
        for &id in ids {
            let Some((t, c)) = self.locate(id) else {
                continue;
            };
            let clip = &mut self.tracks[t].clips[c];
            if !(clip.start < at && at < clip.end()) {
                continue;
            }
            let mut right = clip.clone();
            right.id = ClipId::new();
            right.start = at;
            right.source_in = clip.source_time(at);
            right.duration = clip.end() - at;
            right.group = clip.group.map(|g| *groups.entry(g).or_default());
            clip.duration = at - clip.start;
            created.push(right.id);
            self.tracks[t].clips.insert(c + 1, right);
        }
        created
    }

    /// Range for a clip's left edge: the previous clip and the start of the
    /// source media, and `min` before its right edge.
    fn start_bounds(&self, id: ClipId, min: Time) -> Result<(Time, Time), ProjectError> {
        let (ti, ci) = self.locate(id).ok_or(ProjectError::UnknownClip(id))?;
        let track = &self.tracks[ti];
        let clip = &track.clips[ci];
        let mut lo = if ci > 0 {
            track.clips[ci - 1].end()
        } else {
            Time::ZERO
        };
        if self.source_length(clip).is_some() {
            lo = lo.max(clip.start - clip.source_in);
        }
        Ok((lo, clip.end() - min))
    }

    /// Range for a clip's right edge: `min` after its left edge, and the next
    /// clip and the end of the source media.
    fn end_bounds(&self, id: ClipId, min: Time) -> Result<(Time, Time), ProjectError> {
        let (ti, ci) = self.locate(id).ok_or(ProjectError::UnknownClip(id))?;
        let track = &self.tracks[ti];
        let clip = &track.clips[ci];
        let mut hi = track.clips.get(ci + 1).map_or(UNLIMITED, |n| n.start);
        if let Some(len) = self.source_length(clip) {
            hi = hi.min(clip.start + (len - clip.source_in));
        }
        Ok((clip.start + min, hi))
    }

    /// Moves the left edge of a clip towards `t`, keeping its right edge.
    /// Grouped clips are trimmed by the same amount so they stay in sync.
    /// Edges stop at neighbouring clips and the start of the source media
    /// (these win over the minimum length `min`). Returns the new start.
    pub fn trim_start(&mut self, id: ClipId, t: Time, min: Time) -> Result<Time, ProjectError> {
        let start = self.clip(id).ok_or(ProjectError::UnknownClip(id))?.start;
        let members = self.with_groups(&[id]);
        let (mut lower, mut upper) = (-UNLIMITED, UNLIMITED);
        for m in &members {
            let (lo, hi) = self.start_bounds(*m, min)?;
            let s = self.clip(*m).ok_or(ProjectError::UnknownClip(*m))?.start;
            lower = lower.max(lo - s);
            upper = upper.min(hi - s);
        }
        let delta = towards(t - start, (t - start).min(upper).max(lower));
        for m in members {
            let clip = self.clip_mut(m)?;
            clip.start += delta;
            clip.source_in += delta;
            clip.duration -= delta;
        }
        Ok(start + delta)
    }

    /// Moves the right edge of a clip (and of its group, by the same amount)
    /// towards `t`. Edges stop at the next clip and the end of the source
    /// media, and keep at least `min` length. Returns the new end.
    pub fn trim_end(&mut self, id: ClipId, t: Time, min: Time) -> Result<Time, ProjectError> {
        let end = self.clip(id).ok_or(ProjectError::UnknownClip(id))?.end();
        let members = self.with_groups(&[id]);
        let (mut lower, mut upper) = (-UNLIMITED, UNLIMITED);
        for m in &members {
            let (lo, hi) = self.end_bounds(*m, min)?;
            let e = self.clip(*m).ok_or(ProjectError::UnknownClip(*m))?.end();
            lower = lower.max(lo - e);
            upper = upper.min(hi - e);
        }
        let delta = towards(t - end, (t - end).max(lower).min(upper));
        for m in members {
            self.clip_mut(m)?.duration += delta;
        }
        Ok(end + delta)
    }

    /// Indices into [`Project::tracks`] of all lanes of a kind, bottom to top
    /// for video and top to bottom for audio (i.e. in track order).
    pub fn lanes_of(&self, kind: TrackKind) -> Vec<usize> {
        (0..self.tracks.len())
            .filter(|&i| self.tracks[i].kind == kind)
            .collect()
    }

    /// Moves clips and everything grouped with them by `delta`. The clips in
    /// `ids` also move `lane_shift` lanes within lanes of their kind (in track
    /// order); grouped clips only move in time. Fails without changes on
    /// overlaps, negative start times or missing lanes.
    pub fn move_clips(
        &mut self,
        ids: &[ClipId],
        delta: Time,
        lane_shift: isize,
    ) -> Result<(), ProjectError> {
        let all = self.with_groups(ids);
        let mut moves = Vec::new();
        for id in &all {
            let (t, c) = self.locate(*id).ok_or(ProjectError::UnknownClip(*id))?;
            let mut clip = self.tracks[t].clips[c].clone();
            clip.start += delta;
            if clip.start < Time::ZERO {
                return Err(ProjectError::InvalidEdit("clips cannot start before zero"));
            }
            let mut target = t;
            if ids.contains(id) && lane_shift != 0 {
                let lanes = self.lanes_of(self.tracks[t].kind);
                let pos = lanes.iter().position(|&l| l == t).unwrap_or(0) as isize + lane_shift;
                target = *usize::try_from(pos)
                    .ok()
                    .and_then(|p| lanes.get(p))
                    .ok_or(ProjectError::InvalidEdit("no lane there"))?;
            }
            moves.push((target, clip));
        }
        let backup = self.tracks.clone();
        for track in &mut self.tracks {
            track.clips.retain(|c| !all.contains(&c.id));
        }
        for (target, clip) in moves {
            if let Err(e) = self.tracks[target].insert_sorted(clip) {
                self.tracks = backup;
                return Err(e);
            }
        }
        Ok(())
    }

    /// Deletes clips and everything grouped with them. With `ripple`, later
    /// clips on the same lanes move left to close the gaps; this fails without
    /// changes if it would move grouped clips by different amounts. Returns
    /// the number of removed clips.
    pub fn remove_clips(&mut self, ids: &[ClipId], ripple: bool) -> Result<usize, ProjectError> {
        let all = self.with_groups(ids);
        let backup = self.tracks.clone();
        let mut removed = 0;
        let mut shifts: HashMap<GroupId, Time> = HashMap::new();
        for track in &mut self.tracks {
            let gaps: Vec<(Time, Time)> = track
                .clips
                .iter()
                .filter(|c| all.contains(&c.id))
                .map(|c| (c.end(), c.duration))
                .collect();
            track.clips.retain(|c| !all.contains(&c.id));
            removed += gaps.len();
            if !ripple {
                continue;
            }
            for clip in &mut track.clips {
                let shift = gaps
                    .iter()
                    .filter(|(end, _)| *end <= clip.start)
                    .fold(Time::ZERO, |sum, (_, len)| sum + *len);
                clip.start -= shift;
                if let Some(group) = clip.group
                    && *shifts.entry(group).or_insert(shift) != shift
                {
                    self.tracks = backup;
                    return Err(ProjectError::InvalidEdit(
                        "ripple delete would move linked clips out of sync",
                    ));
                }
            }
        }
        Ok(removed)
    }

    /// Inserts an empty lane at `index` in [`Project::tracks`].
    pub fn insert_track(&mut self, kind: TrackKind, index: usize) -> TrackId {
        let prefix = match kind {
            TrackKind::Video => "V",
            TrackKind::Audio => "A",
        };
        let name = (1..)
            .map(|n| format!("{prefix}{n}"))
            .find(|n| !self.tracks.iter().any(|t| &t.name == n))
            .unwrap_or_default();
        let track = Track::new(kind, name);
        let id = track.id;
        self.tracks.insert(index.min(self.tracks.len()), track);
        id
    }

    pub fn remove_track(&mut self, id: TrackId) -> Result<Track, ProjectError> {
        let i = self
            .tracks
            .iter()
            .position(|t| t.id == id)
            .ok_or(ProjectError::UnknownTrack(id))?;
        Ok(self.tracks.remove(i))
    }

    /// Swaps a lane with the next lane of the same kind in track order
    /// (`forward`) or the previous one. Returns false at the end of the range.
    pub fn move_track(&mut self, id: TrackId, forward: bool) -> bool {
        let Some(i) = self.tracks.iter().position(|t| t.id == id) else {
            return false;
        };
        let lanes = self.lanes_of(self.tracks[i].kind);
        let pos = lanes.iter().position(|&l| l == i).unwrap_or(0);
        let other = if forward {
            lanes.get(pos + 1)
        } else {
            pos.checked_sub(1).and_then(|p| lanes.get(p))
        };
        match other {
            Some(&j) => {
                self.tracks.swap(i, j);
                true
            }
            None => false,
        }
    }

    /// Whether a lane contributes to the output, honouring mute and solo.
    pub fn lane_active(&self, track: &Track) -> bool {
        !track.muted && (track.solo || !self.tracks.iter().any(|t| t.kind == track.kind && t.solo))
    }

    /// First lane of `kind` without clips, adding one (on top of the video
    /// lanes, or below the audio lanes) when every lane is in use.
    pub fn empty_lane(&mut self, kind: TrackKind) -> TrackId {
        if let Some(t) = self
            .tracks
            .iter()
            .find(|t| t.kind == kind && t.clips.is_empty())
        {
            return t.id;
        }
        let index = match kind {
            TrackKind::Video => self.lanes_of(TrackKind::Video).last().map_or(0, |i| i + 1),
            TrackKind::Audio => self.tracks.len(),
        };
        self.insert_track(kind, index)
    }

    /// Places media at `start` on the first empty lane of its kind. Video
    /// with audio also gets an empty audio lane, grouped with the picture.
    pub fn place_media_at(
        &mut self,
        media: MediaId,
        start: Time,
    ) -> Result<Vec<ClipId>, ProjectError> {
        let asset = self.media(media).ok_or(ProjectError::UnknownMedia(media))?;
        let (kind, duration) = (asset.kind(), asset.default_clip_duration());
        let linked = kind == MediaKind::Video && asset.info.audio.is_some();
        let lane = self.empty_lane(match kind {
            MediaKind::Audio => TrackKind::Audio,
            _ => TrackKind::Video,
        });
        let mut clip = Clip::new(media, start, Time::ZERO, duration);
        if !linked {
            return Ok(vec![self.add_clip(lane, clip)?]);
        }
        let audio_lane = self.empty_lane(TrackKind::Audio);
        let group = Some(GroupId::new());
        clip.group = group;
        clip.audio_detached = true;
        let mut audio = Clip::new(media, start, Time::ZERO, duration);
        audio.group = group;
        Ok(vec![
            self.add_clip(lane, clip)?,
            self.add_clip(audio_lane, audio)?,
        ])
    }

    /// Appends media at the end of the timeline. Video with audio is placed on
    /// a video lane and an audio lane at the same time, grouped together.
    pub fn place_media(&mut self, media: MediaId) -> Result<Vec<ClipId>, ProjectError> {
        let asset = self.media(media).ok_or(ProjectError::UnknownMedia(media))?;
        if asset.kind() != MediaKind::Video || asset.info.audio.is_none() {
            let track = self.default_track_for(asset.kind());
            return Ok(vec![self.append_media(track, media)?]);
        }
        let duration = asset.default_clip_duration();
        let video = self.default_track_for(MediaKind::Video);
        let audio = self.default_track_for(MediaKind::Audio);
        let start = [video, audio]
            .iter()
            .filter_map(|t| self.track(*t))
            .map(Track::end)
            .max()
            .unwrap_or(Time::ZERO);
        let group = Some(GroupId::new());
        let mut v = Clip::new(media, start, Time::ZERO, duration);
        v.group = group;
        v.audio_detached = true;
        let mut a = Clip::new(media, start, Time::ZERO, duration);
        a.group = group;
        Ok(vec![self.add_clip(video, v)?, self.add_clip(audio, a)?])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioStreamInfo, MediaAsset, MediaInfo, VideoStreamInfo};

    fn s(x: f64) -> Time {
        Time::from_seconds(x)
    }

    fn asset(kind: MediaKind, secs: Option<f64>, audio: bool) -> MediaAsset {
        MediaAsset::new(
            "m",
            MediaInfo {
                kind,
                duration: secs.map(s),
                video: (kind != MediaKind::Audio).then(|| VideoStreamInfo {
                    width: 64,
                    height: 64,
                    frame_rate: crate::Rational::new(30, 1),
                    codec: "h264".into(),
                }),
                audio: audio.then(|| AudioStreamInfo {
                    sample_rate: 48_000,
                    channels: 2,
                    codec: "aac".into(),
                }),
            },
        )
    }

    /// V1: clip a [0,4) of a 10 s video, clip b [6,8) of an image.
    fn project() -> (Project, ClipId, ClipId) {
        let mut p = Project::new("t");
        let video = p.add_media(asset(MediaKind::Video, Some(10.0), false));
        let image = p.add_media(asset(MediaKind::Image, None, false));
        let v1 = p.tracks[0].id;
        let a = p
            .add_clip(v1, Clip::new(video, s(0.0), s(2.0), s(4.0)))
            .unwrap();
        let b = p
            .add_clip(v1, Clip::new(image, s(6.0), s(0.0), s(2.0)))
            .unwrap();
        (p, a, b)
    }

    #[test]
    fn split_keeps_source_continuity_and_regroups() {
        let (mut p, a, b) = project();
        p.group_clips(&[a, b]).unwrap();
        assert!(p.split_clips(&[a, b], s(8.0)).is_empty());
        let new = p.split_clips(&[a, b], s(1.0));
        assert_eq!(new.len(), 1);
        let (left, right) = (p.clip(a).unwrap(), p.clip(new[0]).unwrap());
        assert_eq!((left.start, left.duration), (s(0.0), s(1.0)));
        assert_eq!(
            (right.start, right.source_in, right.duration),
            (s(1.0), s(3.0), s(3.0))
        );
        assert_ne!(left.group, right.group);
        assert!(right.group.is_some());
        assert!(p.split_clips(&[ClipId::new()], s(1.0)).is_empty());
    }

    #[test]
    fn trims_are_clamped() {
        let (mut p, a, b) = project();
        let min = s(0.5);
        // Source starts 2 s before the clip's first frame.
        assert_eq!(p.trim_start(a, s(-5.0), min).unwrap(), s(0.0));
        assert_eq!(p.trim_start(a, s(1.0), min).unwrap(), s(1.0));
        assert_eq!(p.clip(a).unwrap().source_in, s(3.0));
        assert_eq!(p.trim_start(a, s(9.0), min).unwrap(), s(3.5));
        // End stops at the next clip, then at the end of the source.
        assert_eq!(p.trim_end(a, s(7.0), min).unwrap(), s(6.0));
        p.remove_clips(&[b], false).unwrap();
        assert_eq!(p.trim_end(a, s(20.0), min).unwrap(), s(8.0));
        assert_eq!(p.trim_end(a, s(0.0), min).unwrap(), s(4.0));
        // Images extend freely; the left edge stops at the previous clip.
        let (mut p, a, b) = project();
        assert_eq!(p.trim_end(b, s(100.0), min).unwrap(), s(100.0));
        assert_eq!(
            p.trim_start(b, s(-3.0), min).unwrap(),
            p.clip(a).unwrap().end()
        );
        assert!(matches!(
            p.trim_start(ClipId::new(), s(0.0), min),
            Err(ProjectError::UnknownClip(_))
        ));
        assert!(p.trim_end(ClipId::new(), s(0.0), min).is_err());
        // A fragment shorter than `min` between two clips cannot grow into them.
        let (mut p, a, b) = project();
        p.trim_end(a, s(6.0), min).unwrap();
        let tiny = Time::from_flicks(10);
        let piece = p.split_clips(&[a], s(6.0) - tiny)[0];
        assert_eq!(p.trim_end(piece, s(5.0), min).unwrap(), s(6.0));
        assert_eq!(p.trim_start(piece, s(7.0), min).unwrap(), s(6.0) - tiny);
        assert_eq!(p.clip(b).unwrap().start, s(6.0));
    }

    #[test]
    fn trims_apply_to_the_whole_group() {
        let (mut p, a, b) = project();
        let video = p.clip(a).unwrap().media;
        let a1 = p.tracks[1].id;
        let d = p
            .add_clip(a1, Clip::new(video, s(0.0), s(0.0), s(4.0)))
            .unwrap();
        p.group_clips(&[a, d]).unwrap();
        let min = s(0.5);
        assert_eq!(p.trim_end(a, s(3.0), min).unwrap(), s(3.0));
        assert_eq!(p.clip(d).unwrap().end(), s(3.0));
        assert_eq!(p.trim_start(d, s(1.0), min).unwrap(), s(1.0));
        assert_eq!(
            (p.clip(a).unwrap().start, p.clip(a).unwrap().source_in),
            (s(1.0), s(3.0))
        );
        assert_eq!(p.clip(d).unwrap().source_in, s(1.0));
        // The member with the least room limits the whole group.
        assert_eq!(p.trim_start(a, s(-5.0), min).unwrap(), s(0.0));
        assert_eq!(p.clip(d).unwrap().source_in, s(0.0));
        assert_eq!(p.trim_end(d, s(20.0), min).unwrap(), s(6.0));
        assert_eq!(p.clip(b).unwrap().start, s(6.0));
        // A sub-frame member never makes an inward drag extend the group.
        let tiny = Time::from_flicks(10);
        let piece = p.split_clips(&[d], p.clip(d).unwrap().end() - tiny)[0];
        let other = p.clip(piece).unwrap().group;
        assert!(other.is_some());
        let partner = p.split_clips(&[a], p.clip(a).unwrap().end() - tiny)[0];
        assert_eq!(p.clip(partner).unwrap().group, other);
        let start = p.clip(partner).unwrap().start;
        assert_eq!(p.trim_start(partner, start + s(1.0), min).unwrap(), start);
        let end = p.clip(partner).unwrap().end();
        assert_eq!(p.trim_end(partner, end - s(1.0), min).unwrap(), end);
    }

    #[test]
    fn move_clips_with_groups_and_lanes() {
        let (mut p, a, b) = project();
        let v2 = p.insert_track(TrackKind::Video, 1);
        assert_eq!(p.track(v2).unwrap().name, "V2");
        // Overlap and negative start fail without changes.
        let before = p.clone();
        assert!(matches!(
            p.move_clips(&[a], s(3.0), 0),
            Err(ProjectError::Overlap)
        ));
        assert!(p.move_clips(&[a], s(-1.0), 0).is_err());
        assert!(p.move_clips(&[a], s(0.0), 5).is_err());
        assert!(p.move_clips(&[a], s(0.0), -1).is_err());
        assert_eq!(p, before);
        // To the lane above: the clip moves, its group partner only shifts in time.
        p.group_clips(&[a, b]).unwrap();
        p.move_clips(&[a], s(1.0), 1).unwrap();
        assert_eq!(p.tracks[1].clips[0].id, a);
        assert_eq!(p.clip(a).unwrap().start, s(1.0));
        assert_eq!(p.clip(b).unwrap().start, s(7.0));
        assert_eq!(p.locate(b).unwrap().0, 0);
        assert_eq!(p.ungroup_clips(&[a]), 2);
        assert!(p.group_clips(&[a]).is_err());
    }

    #[test]
    fn remove_with_and_without_ripple() {
        let (mut p, a, b) = project();
        let mut q = p.clone();
        assert_eq!(p.remove_clips(&[a], false).unwrap(), 1);
        assert_eq!(p.clip(b).unwrap().start, s(6.0));
        assert_eq!(q.remove_clips(&[a], true).unwrap(), 1);
        assert_eq!(q.clip(b).unwrap().start, s(2.0));
        // Several gaps before a clip all close.
        let (mut r, a, b) = project();
        let image = r.clip(b).unwrap().media;
        let v1 = r.tracks[0].id;
        let c = r
            .add_clip(v1, Clip::new(image, s(9.0), s(0.0), s(1.0)))
            .unwrap();
        assert_eq!(r.remove_clips(&[a, b], true).unwrap(), 2);
        assert_eq!(r.clip(c).unwrap().start, s(3.0));
        // Linked clips on a lane without the gap would drift: refused.
        let (mut r, a, b) = project();
        let video = r.clip(a).unwrap().media;
        let a1 = r.tracks[1].id;
        let d = r
            .add_clip(a1, Clip::new(video, s(6.0), s(0.0), s(2.0)))
            .unwrap();
        r.group_clips(&[b, d]).unwrap();
        let before = r.clone();
        assert!(r.remove_clips(&[a], true).is_err());
        assert_eq!(r, before);
    }

    #[test]
    fn lanes_solo_mute_and_order() {
        let mut p = Project::new("t");
        let v1 = p.tracks[0].id;
        let v2 = p.insert_track(TrackKind::Video, 1);
        assert_eq!(p.lanes_of(TrackKind::Video), vec![0, 1]);
        assert!(p.move_track(v1, true));
        assert_eq!(p.tracks[1].id, v1);
        assert!(!p.move_track(v1, true));
        assert!(p.move_track(v1, false));
        assert!(!p.move_track(v1, false));
        assert!(!p.move_track(TrackId::new(), false));
        let all = |p: &Project| {
            p.tracks
                .iter()
                .map(|t| p.lane_active(t))
                .collect::<Vec<_>>()
        };
        assert_eq!(all(&p), vec![true, true, true]);
        p.tracks[1].solo = true;
        // Soloing a video lane leaves audio lanes alone.
        assert_eq!(all(&p), vec![false, true, true]);
        p.tracks[2].muted = true;
        assert_eq!(all(&p), vec![false, true, false]);
        assert_eq!(p.remove_track(v2).unwrap().id, v2);
        assert!(p.remove_track(v2).is_err());
    }

    #[test]
    fn place_media_at_uses_empty_lanes() {
        let mut p = Project::new("t");
        let av = p.add_media(asset(MediaKind::Video, Some(3.0), true));
        let still = p.add_media(asset(MediaKind::Image, None, false));
        let tone = p.add_media(asset(MediaKind::Audio, Some(2.0), true));
        let first = p.place_media_at(av, s(1.0)).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(p.clip(first[0]).unwrap().start, s(1.0));
        assert_eq!(p.tracks.len(), 2);
        // Lanes in use: new lanes are created, video on top, audio at the bottom.
        let second = p.place_media_at(av, s(0.0)).unwrap();
        assert_eq!(p.tracks.len(), 4);
        assert_eq!(p.locate(second[0]).unwrap().0, 1);
        assert_eq!(p.locate(second[1]).unwrap().0, 3);
        assert_eq!(p.place_media_at(still, s(0.0)).unwrap().len(), 1);
        assert_eq!(p.lanes_of(TrackKind::Video).len(), 3);
        // An emptied lane is reused.
        let a1 = p.tracks[p.locate(first[1]).unwrap().0].id;
        p.remove_clips(&first, false).unwrap();
        let t = p.place_media_at(tone, s(0.0)).unwrap();
        assert_eq!(p.tracks[p.locate(t[0]).unwrap().0].id, a1);
        assert!(p.place_media_at(MediaId::new(), s(0.0)).is_err());
    }

    #[test]
    fn place_media_groups_video_with_its_audio() {
        let mut p = Project::new("t");
        let av = p.add_media(asset(MediaKind::Video, Some(3.0), true));
        let still = p.add_media(asset(MediaKind::Image, None, false));
        assert_eq!(p.place_media(still).unwrap().len(), 1);
        let ids = p.place_media(av).unwrap();
        let (v, a) = (p.clip(ids[0]).unwrap(), p.clip(ids[1]).unwrap());
        assert_eq!(v.start, a.start);
        assert!(v.audio_detached && !a.audio_detached);
        assert!(v.group.is_some() && v.group == a.group);
        assert_eq!(p.tracks[p.locate(ids[1]).unwrap().0].kind, TrackKind::Audio);
        assert!(p.place_media(MediaId::new()).is_err());
        // Groups survive a JSON round trip; old projects without the fields load.
        let back = Project::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(back, p);
    }
}
