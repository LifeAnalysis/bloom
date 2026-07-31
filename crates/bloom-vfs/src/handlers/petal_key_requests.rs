//! Owner-readable projection of pending/completed Petal key custody requests.
//!
//! Machine writes public reconciliation records into this directory. This
//! handler exposes those exact records read-only through the mounted VFS; it
//! never accepts guest writes and follows no symlinks.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::handler::{Entry, Handler, HandlerError};
use crate::path::VfsPath;

#[derive(Clone, Debug)]
pub struct PetalKeyRequestsHandler {
    root: PathBuf,
}

impl PetalKeyRequestsHandler {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn record_path(&self, path: &VfsPath) -> Result<PathBuf, HandlerError> {
        let [name] = path.segments() else {
            return Err(HandlerError::NotAFile(path.to_string_path()));
        };
        if !valid_record_name(name) {
            return Err(HandlerError::NotFound(path.to_string_path()));
        }
        Ok(self.root.join(name))
    }

    fn regular_metadata(path: &Path) -> Result<std::fs::Metadata, HandlerError> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => Ok(metadata),
            Ok(_) => Err(HandlerError::NotFound(path.display().to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(HandlerError::NotFound(path.display().to_string()))
            }
            Err(error) => Err(error.into()),
        }
    }
}

fn valid_record_name(name: &str) -> bool {
    name.len() == 69
        && name.ends_with(".json")
        && name[..64].bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[async_trait]
impl Handler for PetalKeyRequestsHandler {
    async fn lookup(&self, path: &VfsPath) -> Result<Entry, HandlerError> {
        let record = self.record_path(path)?;
        let metadata = Self::regular_metadata(&record)?;
        let name = path.first().expect("record path contains one segment");
        Ok(Entry::read_only_file(name).with_fs_metadata(&metadata))
    }

    async fn read(&self, path: &VfsPath) -> Result<Vec<u8>, HandlerError> {
        let record = self.record_path(path)?;
        Self::regular_metadata(&record)?;
        std::fs::read(record).map_err(Into::into)
    }

    async fn list(&self, path: &VfsPath) -> Result<Vec<Entry>, HandlerError> {
        if !path.is_root() {
            return Err(HandlerError::NotADir(path.to_string_path()));
        }
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !valid_record_name(&name) {
                continue;
            }
            let metadata = Self::regular_metadata(&entry.path())?;
            records.push(Entry::read_only_file(&name).with_fs_metadata(&metadata));
        }
        records.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn projects_regular_json_records_read_only() {
        let directory = tempfile::tempdir().unwrap();
        let name = format!("{}.json", "ab".repeat(32));
        std::fs::write(directory.path().join(&name), br#"{"state":"pending"}"#).unwrap();
        std::fs::write(directory.path().join("ignored.tmp"), b"secret").unwrap();
        let handler = PetalKeyRequestsHandler::new(directory.path().to_path_buf());
        let entries = handler.list(&VfsPath::root()).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, name);
        assert_eq!(entries[0].mode, 0o444);
        assert_eq!(
            handler.read(&VfsPath::parse(&name).unwrap()).await.unwrap(),
            br#"{"state":"pending"}"#
        );
        assert!(matches!(
            handler
                .write(&VfsPath::parse(&name).unwrap(), b"replace")
                .await,
            Err(HandlerError::PermissionDenied)
        ));
    }
}
