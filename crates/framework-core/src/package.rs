//! The `.fwk` project file: an uncompressed ZIP container holding
//! `project.json` and, after "Collect and save", the media files themselves.
//! Embedded media is stored uncompressed so FFmpeg can read it in place.

use crate::project::{EmbeddedLocation, Project, ProjectError};
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// File extension of Framework projects.
pub const PROJECT_EXTENSION: &str = "fwk";
/// MIME type stored as the first container entry.
pub const PROJECT_MIME: &str = "application/x-framework-project";

const MIMETYPE_ENTRY: &str = "mimetype";
const PROJECT_ENTRY: &str = "project.json";

/// How media is stored when saving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveMode {
    /// Keep references to the original media files (already embedded media stays embedded).
    Reference,
    /// Copy every media file into the project container.
    Collect,
}

impl Project {
    /// Loads a `.fwk` container, or a legacy plain-JSON project.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let path = path.as_ref();
        let mut file = File::open(path)?;
        let mut magic = [0u8; 2];
        let is_zip = file.read_exact(&mut magic).is_ok() && &magic == b"PK";
        if !is_zip {
            return Self::from_json(&fs::read_to_string(path)?);
        }
        file.seek(SeekFrom::Start(0))?;
        let mut archive = ZipArchive::new(BufReader::new(file))?;
        let mut json = String::new();
        archive.by_name(PROJECT_ENTRY)?.read_to_string(&mut json)?;
        let mut project = Self::from_json(&json)?;
        project.locate_embedded(path, &mut archive)?;
        Ok(project)
    }

    /// Saves as a `.fwk` container keeping references to the original media.
    pub fn save(&mut self, path: impl AsRef<Path>) -> Result<(), ProjectError> {
        self.save_as(path, SaveMode::Reference)
    }

    /// Saves as a `.fwk` container. With [`SaveMode::Collect`] all media is
    /// embedded. Afterwards the project refers to the media in the new file.
    pub fn save_as(&mut self, path: impl AsRef<Path>, mode: SaveMode) -> Result<(), ProjectError> {
        let path = path.as_ref();
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let embedded: Vec<_> = self.media.iter().map(|m| m.embedded.clone()).collect();
        if let Err(e) = self.write_container(&tmp, mode) {
            let _ = fs::remove_file(&tmp);
            // Assets collected before the failure are not in any container yet.
            for (asset, entry) in self.media.iter_mut().zip(embedded) {
                asset.embedded = entry;
            }
            return Err(e);
        }
        fs::rename(&tmp, path)?;
        let mut archive = ZipArchive::new(BufReader::new(File::open(path)?))?;
        self.locate_embedded(path, &mut archive)
    }

    fn write_container(&mut self, tmp: &Path, mode: SaveMode) -> Result<(), ProjectError> {
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let mut zip = ZipWriter::new(File::create(tmp)?);
        zip.start_file(MIMETYPE_ENTRY, stored)?;
        zip.write_all(PROJECT_MIME.as_bytes())?;
        for asset in &mut self.media {
            let (entry, mut reader, len) = match (&asset.embedded, &asset.location) {
                (Some(entry), Some(loc)) => {
                    let mut f = File::open(&loc.container)?;
                    f.seek(SeekFrom::Start(loc.start))?;
                    (entry.clone(), f, loc.len)
                }
                (Some(entry), None) => return Err(ProjectError::MissingEmbedded(entry.clone())),
                (None, _) if mode == SaveMode::Collect => {
                    let f = File::open(&asset.path)?;
                    let len = f.metadata()?.len();
                    (
                        format!("media/{}/{}", asset.id, asset.display_name()),
                        f,
                        len,
                    )
                }
                (None, _) => continue,
            };
            let options = stored.large_file(len >= u64::from(u32::MAX));
            zip.start_file(entry.as_str(), options)?;
            let copied = io::copy(&mut (&mut reader).take(len), &mut zip)?;
            if copied != len {
                return Err(ProjectError::MissingEmbedded(entry));
            }
            asset.embedded = Some(entry);
        }
        zip.start_file(PROJECT_ENTRY, stored)?;
        zip.write_all(self.to_json()?.as_bytes())?;
        zip.finish()?.sync_all()?;
        Ok(())
    }

    /// Records where each embedded media entry lives inside `container`.
    fn locate_embedded<R: Read + Seek>(
        &mut self,
        container: &Path,
        archive: &mut ZipArchive<R>,
    ) -> Result<(), ProjectError> {
        let container = fs::canonicalize(container)?;
        for asset in &mut self.media {
            asset.location = None;
            let Some(entry) = &asset.embedded else {
                continue;
            };
            let missing = || ProjectError::MissingEmbedded(entry.clone());
            let file = archive.by_name(entry).map_err(|_| missing())?;
            if file.compression() != CompressionMethod::Stored {
                return Err(missing());
            }
            asset.location = Some(EmbeddedLocation {
                container: container.clone(),
                start: file.data_start().ok_or_else(missing)?,
                len: file.size(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{MediaAsset, MediaInfo, MediaKind};
    use crate::time::Time;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("framework-pkg-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn project_with(media: &Path) -> Project {
        let mut p = Project::new("pkg");
        let info = MediaInfo {
            kind: MediaKind::Audio,
            duration: Some(Time::from_seconds(1.0)),
            video: None,
            audio: None,
        };
        p.add_media(MediaAsset::new(media, info));
        p
    }

    fn embedded_bytes(p: &Project) -> Vec<u8> {
        let loc = p.media[0].location.as_ref().unwrap();
        let mut f = File::open(&loc.container).unwrap();
        f.seek(SeekFrom::Start(loc.start)).unwrap();
        let mut buf = vec![0; loc.len as usize];
        f.read_exact(&mut buf).unwrap();
        buf
    }

    #[test]
    fn reference_save_keeps_paths() {
        let d = dir("ref");
        let media = d.join("a.bin");
        fs::write(&media, b"media").unwrap();
        let mut p = project_with(&media);
        let path = d.join("p.fwk");
        p.save(&path).unwrap();
        assert!(!d.join("p.fwk.tmp").exists());
        let loaded = Project::load(&path).unwrap();
        assert_eq!(loaded.media[0].path, media);
        assert_eq!(loaded.media[0].embedded, None);
        assert_eq!(loaded.media[0].source(), media);
        let mut archive = ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let mut mime = String::new();
        archive
            .by_index(0)
            .unwrap()
            .read_to_string(&mut mime)
            .unwrap();
        assert_eq!(mime, PROJECT_MIME);
    }

    #[test]
    fn collect_embeds_media_and_survives_resave() {
        let d = dir("collect");
        let media = d.join("clip.bin");
        fs::write(&media, b"0123456789").unwrap();
        let mut p = project_with(&media);
        let path = d.join("c.fwk");
        p.save_as(&path, SaveMode::Collect).unwrap();
        assert_eq!(embedded_bytes(&p), b"0123456789");
        fs::remove_file(&media).unwrap();

        let mut loaded = Project::load(&path).unwrap();
        assert!(
            loaded.media[0]
                .embedded
                .as_ref()
                .unwrap()
                .ends_with("/clip.bin")
        );
        assert_eq!(embedded_bytes(&loaded), b"0123456789");
        let source = loaded.media[0].source().to_string_lossy().into_owned();
        assert!(source.starts_with("subfile,,start,"), "{source}");

        // A normal save over the same file keeps the media embedded.
        loaded.name = "renamed".into();
        loaded.save(&path).unwrap();
        assert_eq!(embedded_bytes(&loaded), b"0123456789");
        let again = Project::load(&path).unwrap();
        assert_eq!(again.name, "renamed");
        assert_eq!(embedded_bytes(&again), b"0123456789");
    }

    #[test]
    fn legacy_json_and_errors() {
        let d = dir("legacy");
        let media = d.join("m.bin");
        fs::write(&media, b"x").unwrap();
        let p = project_with(&media);
        let json = d.join("old.json");
        fs::write(&json, p.to_json().unwrap()).unwrap();
        assert_eq!(Project::load(&json).unwrap().media[0].path, media);
        fs::write(d.join("empty"), b"").unwrap();
        assert!(Project::load(d.join("empty")).is_err());

        // Collecting a missing file fails and leaves no temporary file behind.
        let mut missing = project_with(&d.join("gone.bin"));
        let out = d.join("missing.fwk");
        assert!(missing.save_as(&out, SaveMode::Collect).is_err());
        assert!(!d.join("missing.fwk.tmp").exists());

        // A failed collection leaves already collected assets referenced.
        let mut partial = project_with(&media);
        let gone = d.join("gone.bin");
        partial.add_media(MediaAsset::new(&gone, partial.media[0].info.clone()));
        assert!(partial.save_as(&out, SaveMode::Collect).is_err());
        assert!(partial.media.iter().all(|m| m.embedded.is_none()));
        fs::write(&gone, b"back").unwrap();
        partial.save_as(&out, SaveMode::Collect).unwrap();
        assert!(partial.media.iter().all(|m| m.location.is_some()));

        // Embedded media without a container location cannot be saved.
        let mut broken = project_with(&media);
        broken.media[0].embedded = Some("media/x".into());
        assert!(matches!(
            broken.save(&out),
            Err(ProjectError::MissingEmbedded(_))
        ));

        // A container whose project points at a missing entry is rejected.
        let file = File::create(&out).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file(PROJECT_ENTRY, SimpleFileOptions::default())
            .unwrap();
        zip.write_all(broken.to_json().unwrap().as_bytes()).unwrap();
        zip.finish().unwrap();
        assert!(matches!(
            Project::load(&out),
            Err(ProjectError::MissingEmbedded(_))
        ));
    }
}
