//! `docs/` — vendored markdown docs about how to use the FS.

use async_trait::async_trait;

use crate::handler::{Entry, Handler, HandlerError};
use crate::path::VfsPath;

#[derive(Clone)]
pub struct DocsHandler {
    readme: String,
    examples: String,
}

impl Default for DocsHandler {
    fn default() -> Self {
        Self {
            readme: include_str!("../docs/README.md").to_string(),
            examples: include_str!("../docs/examples.md").to_string(),
        }
    }
}

impl DocsHandler {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Handler for DocsHandler {
    async fn lookup(&self, path: &VfsPath) -> Result<Entry, HandlerError> {
        match path.segments() {
            [] => Ok(Entry::dir("")),
            [s] if s == "README.md" => Ok(Entry::file("README.md")),
            [s] if s == "examples.md" => Ok(Entry::file("examples.md")),
            _ => Err(HandlerError::not_found(path.to_string_path())),
        }
    }

    async fn read(&self, path: &VfsPath) -> Result<Vec<u8>, HandlerError> {
        match path.segments() {
            [s] if s == "README.md" => Ok(self.readme.as_bytes().to_vec()),
            [s] if s == "examples.md" => Ok(self.examples.as_bytes().to_vec()),
            _ => Err(HandlerError::NotAFile(path.to_string_path())),
        }
    }

    async fn list(&self, path: &VfsPath) -> Result<Vec<Entry>, HandlerError> {
        if path.is_root() {
            Ok(vec![Entry::file("README.md"), Entry::file("examples.md")])
        } else {
            Err(HandlerError::NotADir(path.to_string_path()))
        }
    }
}
