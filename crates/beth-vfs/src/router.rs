//! Top-level path router. Owns the per-prefix handlers and dispatches.
//!
//! Wires a hash-chained audit log ([`AuditLog`]) into the dispatch path:
//! every successful write appends a `vfs.write` record (with sha256 of
//! the body); every successful read of a *side-effecting* path
//! (handler-declared via [`Handler::is_read_side_effecting`]) appends a
//! `vfs.read` record. Failures are not logged; we err on the side of
//! *fewer* entries to keep the chain useful.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use beth_proto::audit::{AuditLog, AuditRecord};

use crate::handler::{Entry, EntryKind, Handler, HandlerError};
use crate::path::VfsPath;

/// Audit `kind` discriminants. Keep these in sync with `docs/AUDIT.md`.
const AUDIT_KIND_WRITE: &str = "vfs.write";
const AUDIT_KIND_READ: &str = "vfs.read";
/// Default actor recorded for in-process callers. The transport layer
/// (NFS / IPC) doesn't yet thread an authenticated identity through;
/// when it does, plumb it via a request-scoped extension.
const AUDIT_ACTOR_LOCAL: &str = "local";

/// The VFS facade. The daemon constructs one [`Vfs`] and registers a
/// handler for each top-level segment.
#[derive(Clone)]
pub struct Vfs {
    handlers: Arc<BTreeMap<String, Arc<dyn Handler>>>,
    audit: Option<Arc<AuditLog>>,
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
            audit: None,
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

    /// Whether an audit log is wired into this router.
    pub fn has_audit(&self) -> bool {
        self.audit.is_some()
    }

    /// Best-effort audit append. Errors are logged at WARN and dropped —
    /// audit failures must not break user-visible operations.
    fn audit_record(&self, kind: &str, path: &VfsPath, data: serde_json::Value) {
        let Some(log) = &self.audit else {
            return;
        };
        let record = AuditRecord {
            ts_ms: 0, // overwritten by AuditLog::append
            kind: kind.to_string(),
            wallet: None,
            chain: None,
            data: serde_json::json!({
                "path": path.to_string_path(),
                "actor": AUDIT_ACTOR_LOCAL,
                "details": data,
            }),
            prev: String::new(),
            digest: String::new(),
        };
        if let Err(e) = log.append(record) {
            tracing::warn!(target: "beth_vfs::audit", error = %e, "audit append failed");
        }
    }

    fn audit_write(&self, path: &VfsPath, body: &[u8]) {
        let sha = beth_tools::sha256_hex(body);
        self.audit_record(
            AUDIT_KIND_WRITE,
            path,
            serde_json::json!({
                "sha256": sha,
                "size": body.len(),
            }),
        );
    }

    fn audit_side_effecting_read(&self, path: &VfsPath) {
        self.audit_record(AUDIT_KIND_READ, path, serde_json::json!({}));
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
        let rest = path.shift();
        let bytes = h.read(&rest).await?;

        // Side-effecting reads (signing, broadcast triggers, etc) get
        // an audit entry — only on success.
        if h.is_read_side_effecting(&rest) {
            self.audit_side_effecting_read(path);
        }

        Ok(bytes)
    }

    async fn write(&self, path: &VfsPath, data: &[u8]) -> Result<(), HandlerError> {
        let head = path.first().ok_or(HandlerError::PermissionDenied)?;
        let h = self
            .handlers
            .get(head)
            .ok_or_else(|| HandlerError::NotFound(path.to_string_path()))?;
        let rest = path.shift();
        h.write(&rest, data).await?;
        // Successful write — audit it.
        self.audit_write(path, data);
        Ok(())
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
    audit: Option<Arc<AuditLog>>,
}

impl VfsBuilder {
    pub fn mount(mut self, prefix: &str, handler: Arc<dyn Handler>) -> Self {
        self.handlers.insert(prefix.into(), handler);
        self
    }

    /// Wire a hash-chained audit log into the router. Without this,
    /// writes and side-effecting reads run unaudited (back-compat for
    /// tests that don't care).
    pub fn with_audit(mut self, audit: Arc<AuditLog>) -> Self {
        self.audit = Some(audit);
        self
    }

    pub fn build(self) -> Vfs {
        Vfs {
            handlers: Arc::new(self.handlers),
            audit: self.audit,
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// Test handler that counts read/write calls and can be configured
    /// to flag its reads as side-effecting.
    struct CountingHandler {
        side_effecting_read: bool,
        reads: AtomicUsize,
        writes: AtomicUsize,
    }

    impl CountingHandler {
        fn new() -> Self {
            Self {
                side_effecting_read: false,
                reads: AtomicUsize::new(0),
                writes: AtomicUsize::new(0),
            }
        }
        fn with_side_effecting_read(mut self) -> Self {
            self.side_effecting_read = true;
            self
        }
    }

    #[async_trait]
    impl Handler for CountingHandler {
        async fn lookup(&self, p: &VfsPath) -> Result<Entry, HandlerError> {
            Ok(Entry::writable_file(
                p.segments().last().map(|s| s.as_str()).unwrap_or(""),
            ))
        }
        async fn read(&self, _p: &VfsPath) -> Result<Vec<u8>, HandlerError> {
            let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(format!("body-{n}").into_bytes())
        }
        async fn write(&self, _p: &VfsPath, _data: &[u8]) -> Result<(), HandlerError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn is_read_side_effecting(&self, _p: &VfsPath) -> bool {
            self.side_effecting_read
        }
    }

    #[tokio::test]
    async fn write_appends_audit_record() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(AuditLog::open(dir.path().join("audit.jsonl")).unwrap());
        let h = Arc::new(CountingHandler::new());
        let vfs = Vfs::builder().mount("k", h).with_audit(log.clone()).build();
        let p = VfsPath::parse("/k/x").unwrap();
        vfs.write(&p, b"hello").await.unwrap();
        let tail = log.tail(10).unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].kind, AUDIT_KIND_WRITE);
        let details = tail[0].data.get("details").unwrap();
        let sha = details.get("sha256").unwrap().as_str().unwrap();
        assert!(sha.starts_with("0x") && sha.len() == 66, "sha = {sha}");
        assert_eq!(details.get("size").unwrap().as_u64().unwrap(), 5);
        assert_eq!(tail[0].data.get("path").unwrap().as_str().unwrap(), "/k/x");
    }

    #[tokio::test]
    async fn pure_read_does_not_audit_but_side_effecting_does() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(AuditLog::open(dir.path().join("audit.jsonl")).unwrap());
        let pure = Arc::new(CountingHandler::new());
        let signing = Arc::new(CountingHandler::new().with_side_effecting_read());
        let vfs = Vfs::builder()
            .mount("pure", pure)
            .mount("sign", signing)
            .with_audit(log.clone())
            .build();
        vfs.read(&VfsPath::parse("/pure/x").unwrap()).await.unwrap();
        assert_eq!(log.count().unwrap(), 0, "pure read must not audit");
        vfs.read(&VfsPath::parse("/sign/x").unwrap()).await.unwrap();
        assert_eq!(log.count().unwrap(), 1, "side-effecting read must audit");
        let tail = log.tail(1).unwrap();
        assert_eq!(tail[0].kind, AUDIT_KIND_READ);
        assert_eq!(
            tail[0].data.get("path").unwrap().as_str().unwrap(),
            "/sign/x"
        );
    }

    #[tokio::test]
    async fn audit_chain_detects_tamper() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = Arc::new(AuditLog::open(&path).unwrap());
        let h = Arc::new(CountingHandler::new());
        let vfs = Vfs::builder().mount("k", h).with_audit(log).build();
        for body in ["a", "b", "c"] {
            vfs.write(&VfsPath::parse("/k/x").unwrap(), body.as_bytes())
                .await
                .unwrap();
        }
        AuditLog::verify(&path).expect("clean chain verifies");
        // Now tamper with line 1.
        let s = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<&str> = s.lines().collect();
        let mut rec: AuditRecord = serde_json::from_str(lines[0]).unwrap();
        rec.kind = "evil".into();
        let new_first = serde_json::to_string(&rec).unwrap();
        lines[0] = &new_first;
        let body = lines.join("\n") + "\n";
        std::fs::write(&path, body).unwrap();
        assert!(AuditLog::verify(&path).is_err());
    }

    #[tokio::test]
    async fn failed_write_does_not_audit() {
        struct Failing;
        #[async_trait]
        impl Handler for Failing {
            async fn lookup(&self, p: &VfsPath) -> Result<Entry, HandlerError> {
                Ok(Entry::writable_file(p.to_string_path().as_str()))
            }
            async fn write(&self, _p: &VfsPath, _d: &[u8]) -> Result<(), HandlerError> {
                Err(HandlerError::PermissionDenied)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(AuditLog::open(dir.path().join("audit.jsonl")).unwrap());
        let vfs = Vfs::builder()
            .mount("k", Arc::new(Failing))
            .with_audit(log.clone())
            .build();
        let _ = vfs
            .write(&VfsPath::parse("/k/x").unwrap(), b"oops")
            .await
            .unwrap_err();
        assert_eq!(log.count().unwrap(), 0);
    }
}
