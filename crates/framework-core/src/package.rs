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
        let result = self.write_container(&tmp, mode);
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
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
