//! `embednfs::FileSystem` adapter for [`beth_vfs::Vfs`].
//!
//! Maps NFS RPCs onto the four `Handler` async methods (`lookup`,
//! `read`, `write`, `list`). Handles are full VFS paths so attribute
//! lookups stay cheap — the kernel calls `getattr` constantly and we
//! don't want to round-trip through the router twice for every call.
//!
//! ## Path semantics
//!
//! - `BethHandle::Root` corresponds to `/` and is always a directory.
//! - `BethHandle::Path { kind, path }` carries the parsed
//!   [`VfsPath`] plus a cached [`EntryKind`]. The kind is filled in on
//!   `lookup` so subsequent `getattr` calls don't have to ask the VFS
//!   again. Stale-cache risk is acceptable here because the VFS surface
//!   is functionally immutable from the kernel's perspective during a
//!   single mount session — directories don't morph into files.

use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use bytes::Bytes;
use embednfs::{
    AccessMask, Attrs, CreateKind, CreateRequest, CreateResult, DirEntry, DirPage, FileSystem,
    FsError, FsResult, FsStats, ObjectType, ReadResult, RequestContext, SetAttrs, Timestamp,
    WriteResult, WriteStability,
};

use beth_vfs::{Entry, EntryKind, Handler, HandlerError, Vfs, VfsPath};

/// Opaque handle exported over NFS.
///
/// Stable across server restarts within a single process: the kernel
/// caches handles and we want a `getattr` after a `lookup` to keep
/// returning the same object. Handles are equal iff their stringified
/// path is equal (root compares equal to root).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BethHandle {
    Root,
    Path {
        /// Cached kind from the last `lookup`. May lag reality but the
        /// VFS surface is largely static — directories don't become
        /// files mid-session.
        kind: HandleKind,
        path: VfsPath,
    },
}

/// Kind cached on a handle so `getattr` doesn't have to re-query the
/// VFS for object type. Only file/dir/symlink — NFS doesn't carry our
/// finer-grained metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandleKind {
    Dir,
    File,
    Symlink,
}

impl From<EntryKind> for HandleKind {
    fn from(k: EntryKind) -> Self {
        match k {
            EntryKind::Dir => HandleKind::Dir,
            EntryKind::File => HandleKind::File,
            EntryKind::Symlink => HandleKind::Symlink,
        }
    }
}

/// Stable 64-bit fileid derived from the path string. The kernel uses
/// this to keep its inode cache coherent; a deterministic hash means
/// the same VFS path always points at the same `fileid`.
fn fileid_for(path: &VfsPath) -> u64 {
    let s = path.to_string_path();
    let h = blake3_like_hash(s.as_bytes());
    // Reserve 0/1 (bad-fileid sentinels in some clients).
    h.max(2)
}

/// Tiny non-crypto hash. We don't depend on blake3 here to keep the
/// crate's dep set lean — `embednfs` already pulls plenty.
fn blake3_like_hash(bytes: &[u8]) -> u64 {
    // FNV-1a 64-bit. Good enough for fileid stability; collisions are
    // tolerable since the kernel re-resolves on `lookup` mismatch.
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Convert a `HandlerError` from the VFS into the matching NFS error.
fn map_err(e: HandlerError) -> FsError {
    match e {
        HandlerError::NotFound(_) => FsError::NotFound,
        HandlerError::NotADir(_) => FsError::NotDirectory,
        HandlerError::NotAFile(_) => FsError::IsDirectory,
        HandlerError::PermissionDenied => FsError::PermissionDenied,
        HandlerError::Invalid(_) => FsError::InvalidInput,
        HandlerError::Unsupported(_) => FsError::Unsupported,
        HandlerError::Backend(_) => FsError::Io,
        HandlerError::Io(_) => FsError::Io,
    }
}

/// Build a `Timestamp` for "now" (epoch fallback). The VFS doesn't
/// expose mtime per entry today; using the epoch is honest — it tells
/// the kernel "I have no idea, please don't trust this for caching".
fn epoch_ts() -> Timestamp {
    let dur = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp {
        seconds: dur.as_secs() as i64,
        nanos: dur.subsec_nanos(),
    }
}

/// Build attrs for an Entry returned by `list` / `lookup`.
fn entry_to_attrs(path: &VfsPath, e: &Entry) -> Attrs {
    let ot = match e.kind {
        EntryKind::Dir => ObjectType::Directory,
        EntryKind::File => ObjectType::File,
        EntryKind::Symlink => ObjectType::Symlink,
    };
    let mut a = Attrs::new(ot, fileid_for(path));
    a.size = e.size;
    a.space_used = e.size;
    a.mode = e.mode;
    let ts = epoch_ts();
    a.mtime = ts;
    a.atime = ts;
    a.ctime = ts;
    a
}

/// Adapter that holds a clone of the [`Vfs`] facade and serves it as
/// an [`embednfs::FileSystem`].
pub struct BethFs {
    vfs: Vfs,
}

impl BethFs {
    pub fn new(vfs: Vfs) -> Self {
        Self { vfs }
    }

    /// Decompose a handle into the [`VfsPath`] it represents.
    fn path_of(handle: &BethHandle) -> VfsPath {
        match handle {
            BethHandle::Root => VfsPath::root(),
            BethHandle::Path { path, .. } => path.clone(),
        }
    }
}

#[async_trait]
impl FileSystem for BethFs {
    type Handle = BethHandle;

    fn root(&self) -> BethHandle {
        BethHandle::Root
    }

    async fn statfs(&self, _ctx: &RequestContext) -> FsResult<FsStats> {
        Ok(FsStats::default())
    }

    async fn getattr(&self, _ctx: &RequestContext, handle: &BethHandle) -> FsResult<Attrs> {
        match handle {
            BethHandle::Root => {
                let mut a = Attrs::new(ObjectType::Directory, fileid_for(&VfsPath::root()));
                a.mode = 0o755;
                let ts = epoch_ts();
                a.mtime = ts;
                a.atime = ts;
                a.ctime = ts;
                Ok(a)
            }
            BethHandle::Path { kind, path } => {
                // Re-fetch the entry so size/mode reflect current
                // state. `lookup` returns the entry under the same
                // path semantics handlers use.
                let e = self.vfs.lookup(path).await.map_err(map_err)?;
                // If the kind has somehow drifted, prefer the live
                // value over the cached one.
                let _ = kind;
                Ok(entry_to_attrs(path, &e))
            }
        }
    }

    async fn access(
        &self,
        _ctx: &RequestContext,
        handle: &BethHandle,
        requested: AccessMask,
    ) -> FsResult<AccessMask> {
        // Per the v1 spec, the great majority of the tree is read-only
        // (chains/*, status/*, tools/* outputs, prices/*, docs/*, audit
        // views, wallet metadata, watch outputs). Those entries report
        // mode 0o444 from the VFS; only a small handful of injection
        // points (wallets/new, sign/*, outbox writes, watch/new, defi
        // intents new+confirm, policy.toml) report 0o644. Reflect that
        // here so clients see a faithful permission view in `stat` /
        // `access(2)` rather than discovering write rejection only at
        // write-time.
        let mode = match handle {
            BethHandle::Root => 0o755,
            BethHandle::Path { path, .. } => match self.vfs.lookup(path).await {
                Ok(e) => e.mode,
                // If the entry has gone missing between lookup and
                // access, fall through with a permissive mode and let
                // the next op surface the real error.
                Err(_) => 0o644,
            },
        };
        let mut granted = requested;
        // Owner-write bit absent => mask off MODIFY/EXTEND/DELETE.
        if mode & 0o200 == 0 {
            let write_bits = AccessMask::MODIFY | AccessMask::EXTEND | AccessMask::DELETE;
            granted = AccessMask(granted.bits() & !write_bits.bits());
        }
        Ok(granted)
    }

    async fn lookup(
        &self,
        _ctx: &RequestContext,
        parent: &BethHandle,
        name: &str,
    ) -> FsResult<BethHandle> {
        // Reject names that would corrupt the VFS path.
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(FsError::InvalidInput);
        }
        let parent_path = Self::path_of(parent);
        let child = parent_path.join(name);
        let e = self.vfs.lookup(&child).await.map_err(map_err)?;
        Ok(BethHandle::Path {
            kind: HandleKind::from(e.kind),
            path: child,
        })
    }

    async fn parent(
        &self,
        _ctx: &RequestContext,
        dir: &BethHandle,
    ) -> FsResult<Option<BethHandle>> {
        match dir {
            BethHandle::Root => Ok(None),
            BethHandle::Path { path, .. } => {
                let segs = path.segments();
                if segs.len() <= 1 {
                    Ok(Some(BethHandle::Root))
                } else {
                    let parent_str = format!("/{}", segs[..segs.len() - 1].join("/"));
                    let parent = VfsPath::parse(&parent_str).map_err(|_| FsError::InvalidInput)?;
                    Ok(Some(BethHandle::Path {
                        kind: HandleKind::Dir,
                        path: parent,
                    }))
                }
            }
        }
    }

    async fn readdir(
        &self,
        _ctx: &RequestContext,
        dir: &BethHandle,
        cookie: u64,
        max_entries: u32,
        with_attrs: bool,
    ) -> FsResult<DirPage<BethHandle>> {
        let dir_path = Self::path_of(dir);
        let entries = self.vfs.list(&dir_path).await.map_err(map_err)?;

        // Pagination: cookie 0 means "from the start". We hand out
        // dense cookies starting at 3 because 0/1/2 are reserved by
        // the NFSv4 spec for `.` / `..` semantics.
        let start = if cookie == 0 {
            0
        } else {
            cookie.saturating_sub(2) as usize
        };
        let limit = if max_entries == 0 {
            usize::MAX
        } else {
            max_entries as usize
        };
        let total = entries.len();
        let mut out = Vec::new();
        for (idx, e) in entries.into_iter().skip(start).take(limit).enumerate() {
            let child_path = dir_path.join(&e.name);
            let handle = BethHandle::Path {
                kind: HandleKind::from(e.kind),
                path: child_path.clone(),
            };
            let attrs = if with_attrs {
                Some(entry_to_attrs(&child_path, &e))
            } else {
                None
            };
            out.push(DirEntry {
                name: e.name,
                handle,
                cookie: (start + idx + 3) as u64,
                attrs,
            });
        }
        let eof = start + out.len() >= total;
        Ok(DirPage { entries: out, eof })
    }

    async fn read(
        &self,
        _ctx: &RequestContext,
        handle: &BethHandle,
        offset: u64,
        count: u32,
    ) -> FsResult<ReadResult> {
        let path = match handle {
            BethHandle::Root => return Err(FsError::IsDirectory),
            BethHandle::Path { path, .. } => path.clone(),
        };
        let data = self.vfs.read(&path).await.map_err(map_err)?;
        let off = usize::try_from(offset).map_err(|_| FsError::FileTooLarge)?;
        if off >= data.len() {
            return Ok(ReadResult {
                data: Bytes::new(),
                eof: true,
            });
        }
        let end = off.saturating_add(count as usize).min(data.len());
        let chunk = Bytes::copy_from_slice(&data[off..end]);
        Ok(ReadResult {
            data: chunk,
            eof: end == data.len(),
        })
    }

    async fn write(
        &self,
        _ctx: &RequestContext,
        handle: &BethHandle,
        offset: u64,
        data: Bytes,
        requested: WriteStability,
    ) -> FsResult<WriteResult> {
        // We don't buffer partial writes — the VFS write API is
        // whole-file at v1, and most writable paths (wallet outbox,
        // watch subscriptions) want atomic semantics anyway. Reject a
        // non-zero offset rather than silently dropping bytes.
        if offset != 0 {
            return Err(FsError::Unsupported);
        }
        let path = match handle {
            BethHandle::Root => return Err(FsError::IsDirectory),
            BethHandle::Path { path, .. } => path.clone(),
        };
        let len = data.len();
        self.vfs.write(&path, &data).await.map_err(map_err)?;
        Ok(WriteResult {
            written: u32::try_from(len).unwrap_or(u32::MAX),
            stability: requested,
        })
    }

    async fn create(
        &self,
        _ctx: &RequestContext,
        parent: &BethHandle,
        name: &str,
        req: CreateRequest,
    ) -> FsResult<CreateResult<BethHandle>> {
        // VFS doesn't expose a create-empty op; for files we issue a
        // zero-byte write (writable handlers are expected to materialise
        // an entry). Directory creation is not supported in v1 — the
        // VFS structure is fixed.
        if matches!(req.kind, CreateKind::Directory) {
            return Err(FsError::Unsupported);
        }
        if name.is_empty() || name.contains('/') {
            return Err(FsError::InvalidInput);
        }
        let parent_path = Self::path_of(parent);
        let child = parent_path.join(name);
        self.vfs.write(&child, &[]).await.map_err(map_err)?;
        let e = self.vfs.lookup(&child).await.map_err(map_err)?;
        let attrs = entry_to_attrs(&child, &e);
        let handle = BethHandle::Path {
            kind: HandleKind::from(e.kind),
            path: child,
        };
        Ok(CreateResult { handle, attrs })
    }

    async fn remove(
        &self,
        _ctx: &RequestContext,
        _parent: &BethHandle,
        _name: &str,
    ) -> FsResult<()> {
        // VFS is append/overwrite only in v1.
        Err(FsError::Unsupported)
    }

    async fn rename(
        &self,
        _ctx: &RequestContext,
        _from_dir: &BethHandle,
        _from_name: &str,
        _to_dir: &BethHandle,
        _to_name: &str,
    ) -> FsResult<()> {
        Err(FsError::Unsupported)
    }

    async fn setattr(
        &self,
        ctx: &RequestContext,
        handle: &BethHandle,
        _attrs: &SetAttrs,
    ) -> FsResult<Attrs> {
        // No-op: refresh attrs from the VFS and return them.
        self.getattr(ctx, handle).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use beth_vfs::handler::{Entry, Handler, HandlerError};

    struct StaticHandler;

    #[async_trait]
    impl Handler for StaticHandler {
        async fn lookup(&self, p: &VfsPath) -> Result<Entry, HandlerError> {
            if p.is_root() {
                return Ok(Entry::dir(""));
            }
            match p.first() {
                Some("hello") => Ok(Entry::file("hello")),
                _ => Err(HandlerError::NotFound(p.to_string_path())),
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

    fn fake_ctx() -> RequestContext {
        // RequestContext is constructed by the embednfs server in real
        // use; for adapter unit tests we only ever read it via
        // `_ctx`-prefixed args, so an anonymous one is fine.
        RequestContext::anonymous()
    }

    #[tokio::test]
    async fn root_lookup_returns_directory() {
        let vfs = Vfs::builder()
            .mount("echo", Arc::new(StaticHandler))
            .build();
        let fs = BethFs::new(vfs);
        let ctx = fake_ctx();
        let attrs = fs.getattr(&ctx, &BethHandle::Root).await.unwrap();
        assert_eq!(attrs.object_type, ObjectType::Directory);
    }

    #[tokio::test]
    async fn lookup_then_read_yields_file_contents() {
        let vfs = Vfs::builder()
            .mount("echo", Arc::new(StaticHandler))
            .build();
        let fs = BethFs::new(vfs);
        let ctx = fake_ctx();
        let echo = fs.lookup(&ctx, &BethHandle::Root, "echo").await.unwrap();
        let hello = fs.lookup(&ctx, &echo, "hello").await.unwrap();
        let r = fs.read(&ctx, &hello, 0, 1024).await.unwrap();
        assert_eq!(&r.data[..], b"world\n");
        assert!(r.eof);
    }

    #[tokio::test]
    async fn readdir_root_lists_handlers() {
        let vfs = Vfs::builder()
            .mount("echo", Arc::new(StaticHandler))
            .build();
        let fs = BethFs::new(vfs);
        let ctx = fake_ctx();
        let page = fs
            .readdir(&ctx, &BethHandle::Root, 0, 100, true)
            .await
            .unwrap();
        let names: Vec<&str> = page.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["echo"]);
        assert!(page.eof);
    }
}
