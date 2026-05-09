//! Top-level path router. Owns the per-prefix handlers and dispatches.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::handler::{Entry, EntryKind, Handler, HandlerError};
use crate::path::VfsPath;

/// The VFS facade. The daemon constructs one [`Vfs`] and registers a
/// handler for each top-level segment.
#[derive(Clone)]
pub struct Vfs {
    handlers: Arc<BTreeMap<String, Arc<dyn Handler>>>,
}

impl Default for Vfs {
    fn default() -> Self {
        Self::new()
    }
}

impl Vfs {
    pub fn new() -> Self {
        Self {
            handlers: Arc::new(BTreeMap::new()),
        }
    }

    pub fn builder() -> VfsBuilder {
        VfsBuilder::default()
    }

    pub fn handler(&self, name: &str) -> Option<&Arc<dyn Handler>> {
        self.handlers.get(name)
    }

    pub fn top_segments(&self) -> Vec<String> {
        self.handlers.keys().cloned().collect()
    }
}

#[async_trait]
impl Handler for Vfs {
    async fn lookup(&self, path: &VfsPath) -> Result<Entry, HandlerError> {
        if path.is_root() {
            return Ok(Entry::dir(""));
        }
        let head = path.first().unwrap();
        let h = self
            .handlers
            .get(head)
            .ok_or_else(|| HandlerError::NotFound(path.to_string_path()))?;
        let rest = path.shift();
        if rest.is_root() {
            return Ok(Entry::dir(head));
        }
        h.lookup(&rest).await
    }

    async fn read(&self, path: &VfsPath) -> Result<Vec<u8>, HandlerError> {
        let head = path
            .first()
            .ok_or_else(|| HandlerError::NotAFile(path.to_string_path()))?;
        let h = self
            .handlers
            .get(head)
            .ok_or_else(|| HandlerError::NotFound(path.to_string_path()))?;
        h.read(&path.shift()).await
    }

    async fn write(&self, path: &VfsPath, data: &[u8]) -> Result<(), HandlerError> {
        let head = path.first().ok_or(HandlerError::PermissionDenied)?;
        let h = self
            .handlers
            .get(head)
            .ok_or_else(|| HandlerError::NotFound(path.to_string_path()))?;
        h.write(&path.shift(), data).await
    }

    async fn list(&self, path: &VfsPath) -> Result<Vec<Entry>, HandlerError> {
        if path.is_root() {
            let mut out = Vec::new();
            for name in self.handlers.keys() {
                out.push(Entry::dir(name));
            }
            return Ok(out);
        }
        let head = path.first().unwrap();
        let h = self
            .handlers
            .get(head)
            .ok_or_else(|| HandlerError::NotFound(path.to_string_path()))?;
        let rest = path.shift();
        let entries = h.list(&rest).await?;
        Ok(entries)
    }
}

#[derive(Default)]
pub struct VfsBuilder {
    handlers: BTreeMap<String, Arc<dyn Handler>>,
}

impl VfsBuilder {
    pub fn mount(mut self, prefix: &str, handler: Arc<dyn Handler>) -> Self {
        self.handlers.insert(prefix.into(), handler);
        self
    }

    pub fn build(self) -> Vfs {
        Vfs {
            handlers: Arc::new(self.handlers),
        }
    }
}

/// Convenience: render a value as an `ls -l`-style metadata line.
pub fn entry_size(e: &Entry) -> u64 {
    e.size
}

/// Convenience: classify entry as dir.
pub fn is_dir(e: &Entry) -> bool {
    e.kind == EntryKind::Dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::Entry;

    struct EchoHandler;

    #[async_trait]
    impl Handler for EchoHandler {
        async fn lookup(&self, p: &VfsPath) -> Result<Entry, HandlerError> {
            if p.is_root() {
                Ok(Entry::dir(""))
            } else if p.segments().last().map(|s| s.as_str()) == Some("hello") {
                Ok(Entry::file("hello"))
            } else {
                Err(HandlerError::NotFound(p.to_string_path()))
            }
        }
        async fn read(&self, _p: &VfsPath) -> Result<Vec<u8>, HandlerError> {
            Ok(b"world\n".to_vec())
        }
        async fn list(&self, p: &VfsPath) -> Result<Vec<Entry>, HandlerError> {
            if p.is_root() {
                Ok(vec![Entry::file("hello")])
            } else {
                Err(HandlerError::NotADir(p.to_string_path()))
            }
        }
    }

    #[tokio::test]
    async fn dispatches_to_handler() {
        let vfs = Vfs::builder().mount("echo", Arc::new(EchoHandler)).build();
        let p = VfsPath::parse("/echo/hello").unwrap();
        let e = vfs.lookup(&p).await.unwrap();
        assert_eq!(e.kind, EntryKind::File);
        let body = vfs.read(&p).await.unwrap();
        assert_eq!(body, b"world\n");
    }

    #[tokio::test]
    async fn root_lists_top_segments() {
        let vfs = Vfs::builder().mount("echo", Arc::new(EchoHandler)).build();
        let entries = vfs.list(&VfsPath::root()).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "echo");
    }

    #[tokio::test]
    async fn unknown_prefix_not_found() {
        let vfs = Vfs::builder().mount("echo", Arc::new(EchoHandler)).build();
        let r = vfs.lookup(&VfsPath::parse("/nope").unwrap()).await;
        assert!(matches!(r, Err(HandlerError::NotFound(_))));
    }
}
