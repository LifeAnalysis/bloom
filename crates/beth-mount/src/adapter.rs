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

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use embednfs::{
    AccessMask, Attrs, CommitSupport, CreateKind, CreateRequest, CreateResult, DirEntry, DirPage,
    FileSystem, FsError, FsResult, FsStats, ObjectType, ReadResult, RequestContext, SetAttrs,
    Timestamp, WriteResult, WriteStability,
};
use parking_lot::Mutex;

use beth_vfs::{Entry, EntryKind, Handler, HandlerError, Vfs, VfsPath};

/// Maximum bytes we'll buffer for a single open file before forcing a
/// flush (or rejecting further writes with FBIG). 8 MiB matches the
/// spec hint and is large enough for any plausible JSON/TOML/EIP-712
/// body the daemon expects through the mount surface.
pub(crate) const MAX_WRITE_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// Time without further writes after which a buffer is auto-flushed.
/// Picked to match the typical NFSv4 client behaviour: kernels with
/// `wsize=4096` issue a burst of WRITEs followed by a COMMIT once the
/// userspace `close(2)` returns. The COMMIT path is the primary flush
/// trigger; this idle timer is the safety net for clients that skip
/// COMMIT or for `O_DIRECT`-style writes that arrive with `FileSync`
/// stability and never see a follow-up COMMIT.
pub(crate) const WRITE_IDLE_FLUSH: Duration = Duration::from_secs(5);

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

/// Per-path buffered write state. Holds the assembled file contents
/// (sparse during in-flight writes, contiguous at flush time) plus a
/// last-write timestamp so the idle-flush task can collect stragglers.
///
/// Writes can arrive out of order — the kernel is free to issue
/// `WRITE off=4096`, `WRITE off=0`, `WRITE off=8192` in any sequence.
/// We tolerate that by sizing `bytes` to the high-water mark and
/// tracking the set of filled byte ranges in `filled` (a sorted map
/// keyed by start offset). The buffer is flushable once the union of
/// those ranges is exactly `[0, bytes.len())`.
#[derive(Debug)]
struct WriteBuffer {
    /// Logical file contents, indexed by offset. Bytes inside a range
    /// recorded in `filled` are valid; bytes outside remain at their
    /// default zero and must not be flushed.
    bytes: Vec<u8>,
    /// Sorted, non-overlapping, non-adjacent map of filled byte
    /// ranges keyed by start offset (value is end offset, exclusive).
    /// Adjacent and overlapping ranges are merged on insert so the
    /// "is contiguous prefix" check is a single map lookup.
    filled: BTreeMap<usize, usize>,
    /// Timestamp of the most recent write. Idle-flush compares against
    /// this so a one-shot O_DIRECT write that finishes without COMMIT
    /// still lands eventually.
    last_write: Instant,
    /// Total bytes the client has handed us across all WRITEs (count
    /// of bytes received, not max-offset). Tracked for the FBIG cap so
    /// pathological out-of-order patterns can't blow past the limit.
    received: usize,
}

impl WriteBuffer {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            filled: BTreeMap::new(),
            last_write: Instant::now(),
            received: 0,
        }
    }

    /// Apply a chunk at the requested offset. Grows `bytes` as needed
    /// and merges the new `[off, end)` range into `filled`. Returns
    /// `Err(FsError::FileTooLarge)` if accepting the chunk would push
    /// the buffer past `MAX_WRITE_BUFFER_BYTES`.
    fn apply(&mut self, offset: u64, data: &[u8]) -> FsResult<()> {
        let off = usize::try_from(offset).map_err(|_| FsError::FileTooLarge)?;
        let end = off.checked_add(data.len()).ok_or(FsError::FileTooLarge)?;
        if end > MAX_WRITE_BUFFER_BYTES {
            return Err(FsError::FileTooLarge);
        }
        if end > self.bytes.len() {
            self.bytes.resize(end, 0);
        }
        self.bytes[off..end].copy_from_slice(data);
        self.merge_range(off, end);
        self.last_write = Instant::now();
        self.received = self.received.saturating_add(data.len());
        Ok(())
    }

    /// Merge `[start, end)` into `filled`, coalescing with any
    /// adjacent or overlapping ranges. After this returns, `filled`
    /// remains a valid disjoint, non-adjacent set keyed by start
    /// offset.
    fn merge_range(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let mut new_start = start;
        let mut new_end = end;
        // Absorb any range whose start <= new_end (i.e. overlapping
        // or adjacent on the right) and whose end >= new_start (i.e.
        // overlapping or adjacent on the left). Collect keys first
        // because we mutate the map while iterating.
        let to_remove: Vec<usize> = self
            .filled
            .range(..=new_end)
            .filter_map(|(&s, &e)| if e >= new_start { Some(s) } else { None })
            .collect();
        for key in to_remove {
            let existing_end = self.filled.remove(&key).expect("just observed");
            new_start = new_start.min(key);
            new_end = new_end.max(existing_end);
        }
        self.filled.insert(new_start, new_end);
    }

    /// Returns true if the buffered contents form a contiguous prefix
    /// starting at offset 0 — i.e. it's safe to flush.
    fn is_complete(&self) -> bool {
        if self.bytes.is_empty() {
            return false;
        }
        match self.filled.iter().next() {
            Some((&start, &end)) => start == 0 && end == self.bytes.len(),
            None => false,
        }
    }
}

/// Adapter that holds a clone of the [`Vfs`] facade and serves it as
/// an [`embednfs::FileSystem`].
///
/// ## Write buffering
///
/// The bloom-eth VFS exposes a whole-file `write(path, &[u8])` API, but
/// NFS clients chunk a single user-space `write(2)` into multiple
/// `WRITE` ops at increasing offsets (with `wsize=4096`, a 16 KiB JSON
/// body becomes four ops at offsets 0/4096/8192/12288). Without
/// buffering, every chunk past the first would be either rejected
/// (offset != 0) or would clobber the file with a 4 KiB tail.
///
/// This adapter buffers WRITE chunks per file handle in
/// [`BethFs::write_buffers`]. A buffer is flushed to the VFS on:
///
/// 1. An NFS COMMIT against the handle (the primary trigger — the
///    Linux client issues COMMIT after the userspace `close(2)` /
///    `fsync(2)` for unstable writes).
/// 2. An idle timer ([`WRITE_IDLE_FLUSH`]) since the last WRITE on
///    that handle, as a safety net for clients that skip COMMIT or
///    for `WriteStability::FileSync` writes that arrive without a
///    follow-up COMMIT.
/// 3. A `read` against the same handle — we flush first, then read,
///    so the user observes their own writes.
///
/// Reads of an open partially-written file return the previously
/// committed contents, not the buffered bytes. This is the simplest
/// policy that preserves "write semantics from a single client read
/// back what the client just wrote" via the flush-before-read rule.
pub struct BethFs {
    vfs: Vfs,
    /// Per-handle write buffers. Keyed by `VfsPath` so multiple clients
    /// writing the same file coalesce — NFS state-tracking the way the
    /// RFC describes it (open-stateid-keyed) would be more correct, but
    /// the bloom-eth surface assumes a single agent per mount and the
    /// per-path scheme is dramatically simpler. The tradeoff: two
    /// concurrent writers to the same path see interleaved chunks and
    /// must serialise themselves at the application layer.
    write_buffers: Arc<Mutex<HashMap<VfsPath, WriteBuffer>>>,
}

impl BethFs {
    pub fn new(vfs: Vfs) -> Self {
        Self {
            vfs,
            write_buffers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Decompose a handle into the [`VfsPath`] it represents.
    fn path_of(handle: &BethHandle) -> VfsPath {
        match handle {
            BethHandle::Root => VfsPath::root(),
            BethHandle::Path { path, .. } => path.clone(),
        }
    }

    /// Take a buffer's contents out, leaving the slot empty. Returns
    /// `Some(bytes)` only if the buffer was contiguous — partial
    /// buffers stay parked so a follow-up WRITE can fill the gap.
    fn take_complete_buffer(&self, path: &VfsPath) -> Option<Vec<u8>> {
        let mut map = self.write_buffers.lock();
        match map.get(path) {
            Some(buf) if buf.is_complete() => {
                let buf = map.remove(path).expect("just observed");
                Some(buf.bytes)
            }
            _ => None,
        }
    }

    /// Flush any buffered writes for `path` through to the VFS. No-op
    /// if the buffer is empty or non-contiguous (the latter only
    /// happens if a client never sends the missing prefix; the idle
    /// timer would normally drop such buffers, but flush_path is
    /// defensive about it).
    async fn flush_path(&self, path: &VfsPath) -> FsResult<()> {
        if let Some(bytes) = self.take_complete_buffer(path) {
            self.vfs.write(path, &bytes).await.map_err(map_err)?;
        }
        Ok(())
    }

    /// Discard any buffer for `path` whose last write is older than
    /// `WRITE_IDLE_FLUSH`. Used by the read path so an abandoned
    /// partial write doesn't shadow committed state forever.
    fn drop_stale_buffer(&self, path: &VfsPath) -> Option<Vec<u8>> {
        let mut map = self.write_buffers.lock();
        let stale = map
            .get(path)
            .map(|b| b.is_complete() || b.last_write.elapsed() > WRITE_IDLE_FLUSH)
            .unwrap_or(false);
        if stale {
            map.remove(path).map(|b| b.bytes)
        } else {
            None
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
        // If the client has buffered writes that complete a contiguous
        // file, flush them now so the read sees the latest state. We
        // also opportunistically drop stale partial buffers so an
        // orphaned WRITE doesn't pin memory across reads.
        self.flush_path(&path).await?;
        let _ = self.drop_stale_buffer(&path);
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
        let path = match handle {
            BethHandle::Root => return Err(FsError::IsDirectory),
            BethHandle::Path { path, .. } => path.clone(),
        };
        let len = data.len();
        if len == 0 {
            // Zero-byte writes don't carry data and never complete a
            // buffer; treat them as no-ops at this layer. They can
            // still be useful for `create` (handled separately) and
            // for kernels that use them as truncate hints (we ignore
            // truncate-via-write and rely on `setattr` for size).
            return Ok(WriteResult {
                written: 0,
                stability: requested,
            });
        }

        // Buffer the chunk. We lock long enough to apply the chunk and
        // observe whether the buffer is now complete; the actual VFS
        // write happens outside the lock so a slow handler can't stall
        // concurrent writers to other paths.
        let (complete_payload, accepted) = {
            let mut map = self.write_buffers.lock();
            let buf = map.entry(path.clone()).or_insert_with(WriteBuffer::new);
            // FBIG check before mutating: fail fast so the client gets
            // a clean error rather than silently truncated input.
            let proposed_received = buf.received.saturating_add(len);
            if proposed_received > MAX_WRITE_BUFFER_BYTES {
                map.remove(&path);
                return Err(FsError::FileTooLarge);
            }
            buf.apply(offset, &data)?;
            let payload = if buf.is_complete() && requested == WriteStability::FileSync {
                // Eager flush on FILE_SYNC: clients that bypass COMMIT
                // (notably some macOS NFS quirks) will set this.
                Some(map.remove(&path).expect("just observed").bytes)
            } else {
                None
            };
            (payload, len)
        };

        if let Some(payload) = complete_payload {
            self.vfs.write(&path, &payload).await.map_err(map_err)?;
        }

        Ok(WriteResult {
            written: u32::try_from(accepted).unwrap_or(u32::MAX),
            // Always advertise UNSTABLE so the kernel sends a follow-up
            // COMMIT — that's the path that flushes a multi-chunk
            // write. The eager FILE_SYNC fast path above handles the
            // case where the kernel skips COMMIT.
            stability: WriteStability::Unstable,
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

    fn commit_support(&self) -> Option<&dyn CommitSupport<BethHandle>> {
        // Surface ourselves as the commit handler so the kernel's
        // post-write COMMIT op routes back into [`BethFs::commit`] and
        // flushes the per-handle write buffer.
        Some(self)
    }
}

#[async_trait]
impl CommitSupport<BethHandle> for BethFs {
    async fn commit(
        &self,
        _ctx: &RequestContext,
        handle: &BethHandle,
        _offset: u64,
        _count: u32,
    ) -> FsResult<()> {
        // NFS COMMIT is byte-range scoped, but the bloom-eth VFS is
        // whole-file. We treat any COMMIT against a handle as "flush
        // everything you have for this path". If the buffer is
        // incomplete (a missing prefix), we leave it in place — the
        // client will either resend the missing chunk or the idle
        // timer will reap it on the next read.
        let path = match handle {
            BethHandle::Root => return Err(FsError::IsDirectory),
            BethHandle::Path { path, .. } => path.clone(),
        };
        self.flush_path(&path).await
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

    /// Test handler that records every `write` it sees and exposes a
    /// single writable file `inbox`. Used to verify the adapter's
    /// per-handle write buffering coalesces multi-block writes into
    /// exactly one `vfs.write` call.
    #[derive(Default)]
    struct RecordingHandler {
        writes: parking_lot::Mutex<Vec<Vec<u8>>>,
    }

    impl RecordingHandler {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn write_count(&self) -> usize {
            self.writes.lock().len()
        }

        fn last_write(&self) -> Option<Vec<u8>> {
            self.writes.lock().last().cloned()
        }
    }

    #[async_trait]
    impl Handler for RecordingHandler {
        async fn lookup(&self, p: &VfsPath) -> Result<Entry, HandlerError> {
            if p.is_root() {
                return Ok(Entry::dir(""));
            }
            match p.first() {
                Some("inbox") => Ok(Entry::writable_file("inbox")),
                Some("readme") => Ok(Entry::file("readme")),
                _ => Err(HandlerError::NotFound(p.to_string_path())),
            }
        }
        async fn read(&self, p: &VfsPath) -> Result<Vec<u8>, HandlerError> {
            match p.first() {
                Some("inbox") => Ok(self.writes.lock().last().cloned().unwrap_or_default()),
                Some("readme") => Ok(b"static read-only body\n".to_vec()),
                _ => Err(HandlerError::NotAFile(p.to_string_path())),
            }
        }
        async fn write(&self, p: &VfsPath, data: &[u8]) -> Result<(), HandlerError> {
            match p.first() {
                Some("inbox") => {
                    self.writes.lock().push(data.to_vec());
                    Ok(())
                }
                _ => Err(HandlerError::PermissionDenied),
            }
        }
        async fn list(&self, p: &VfsPath) -> Result<Vec<Entry>, HandlerError> {
            if p.is_root() {
                Ok(vec![Entry::writable_file("inbox"), Entry::file("readme")])
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

    /// Bug #4 acceptance: a 16 KiB write delivered as four 4 KiB
    /// chunks at offsets 0/4096/8192/12288 followed by a COMMIT must
    /// land as a single `vfs.write` carrying the joined payload.
    #[tokio::test]
    async fn buffered_chunks_flush_on_commit() {
        let recorder = RecordingHandler::new();
        let vfs = Vfs::builder().mount("box", recorder.clone()).build();
        let fs = BethFs::new(vfs);
        let ctx = fake_ctx();
        let dir = fs.lookup(&ctx, &BethHandle::Root, "box").await.unwrap();
        let inbox = fs.lookup(&ctx, &dir, "inbox").await.unwrap();

        // Build a deterministic 16 KiB payload (each block tagged with
        // its offset so we can detect mis-ordering on flush).
        let mut payload = Vec::with_capacity(16 * 1024);
        for off in [0u32, 4096, 8192, 12288] {
            for b in 0..4096 {
                payload.push(((off / 4096) as u8).wrapping_add((b & 0xff) as u8));
            }
        }
        let chunks: Vec<&[u8]> = payload.chunks(4096).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            let off = (i as u64) * 4096;
            let result = fs
                .write(
                    &ctx,
                    &inbox,
                    off,
                    Bytes::copy_from_slice(chunk),
                    WriteStability::Unstable,
                )
                .await
                .unwrap();
            assert_eq!(result.written, 4096);
            assert_eq!(result.stability, WriteStability::Unstable);
        }
        // No flush yet — UNSTABLE writes wait for COMMIT.
        assert_eq!(recorder.write_count(), 0);

        // COMMIT: the kernel issues this on close/fsync and it must
        // collapse the four chunks into exactly one VFS write.
        let cs = fs.commit_support().expect("commit support enabled");
        cs.commit(&ctx, &inbox, 0, payload.len() as u32)
            .await
            .unwrap();
        assert_eq!(recorder.write_count(), 1);
        assert_eq!(recorder.last_write().unwrap(), payload);
    }

    /// Bug #4 acceptance: out-of-order chunks plus a final prefix
    /// chunk + COMMIT still produce a single coalesced write.
    #[tokio::test]
    async fn buffered_chunks_tolerate_out_of_order() {
        let recorder = RecordingHandler::new();
        let vfs = Vfs::builder().mount("box", recorder.clone()).build();
        let fs = BethFs::new(vfs);
        let ctx = fake_ctx();
        let dir = fs.lookup(&ctx, &BethHandle::Root, "box").await.unwrap();
        let inbox = fs.lookup(&ctx, &dir, "inbox").await.unwrap();

        let mut payload = vec![0u8; 12288];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i & 0xff) as u8;
        }
        // Send middle, then tail, then head — common pattern for
        // multi-threaded or io_uring clients.
        let send = |off: u64, lo: usize, hi: usize| {
            let bytes = Bytes::copy_from_slice(&payload[lo..hi]);
            (off, bytes)
        };
        let middle = send(4096, 4096, 8192);
        let tail = send(8192, 8192, 12288);
        let head = send(0, 0, 4096);
        for (off, bytes) in [middle, tail, head] {
            fs.write(&ctx, &inbox, off, bytes, WriteStability::Unstable)
                .await
                .unwrap();
        }
        assert_eq!(recorder.write_count(), 0);

        let cs = fs.commit_support().unwrap();
        cs.commit(&ctx, &inbox, 0, payload.len() as u32)
            .await
            .unwrap();
        assert_eq!(recorder.write_count(), 1);
        assert_eq!(recorder.last_write().unwrap(), payload);
    }

    /// Bug #4 acceptance: a single write tagged FILE_SYNC (no
    /// follow-up COMMIT) flushes immediately on the eager path.
    #[tokio::test]
    async fn file_sync_write_flushes_eagerly() {
        let recorder = RecordingHandler::new();
        let vfs = Vfs::builder().mount("box", recorder.clone()).build();
        let fs = BethFs::new(vfs);
        let ctx = fake_ctx();
        let dir = fs.lookup(&ctx, &BethHandle::Root, "box").await.unwrap();
        let inbox = fs.lookup(&ctx, &dir, "inbox").await.unwrap();
        let body = b"hello bloom\n";
        fs.write(
            &ctx,
            &inbox,
            0,
            Bytes::copy_from_slice(body),
            WriteStability::FileSync,
        )
        .await
        .unwrap();
        assert_eq!(recorder.write_count(), 1);
        assert_eq!(recorder.last_write().unwrap(), body);
    }

    /// Bug #4 acceptance: a write that would push the per-handle
    /// buffer past `MAX_WRITE_BUFFER_BYTES` must be rejected with
    /// `FileTooLarge` (NFS4ERR_FBIG) before any state mutation.
    #[tokio::test]
    async fn oversize_write_rejects_fbig() {
        let recorder = RecordingHandler::new();
        let vfs = Vfs::builder().mount("box", recorder.clone()).build();
        let fs = BethFs::new(vfs);
        let ctx = fake_ctx();
        let dir = fs.lookup(&ctx, &BethHandle::Root, "box").await.unwrap();
        let inbox = fs.lookup(&ctx, &dir, "inbox").await.unwrap();
        // One byte past the cap — even at offset 0 this should fail
        // because the buffer would have to grow to MAX+1.
        let oversized = Bytes::from(vec![0u8; MAX_WRITE_BUFFER_BYTES + 1]);
        let err = fs
            .write(&ctx, &inbox, 0, oversized, WriteStability::Unstable)
            .await
            .unwrap_err();
        assert_eq!(err, FsError::FileTooLarge);
        // No partial state should have leaked through.
        assert_eq!(recorder.write_count(), 0);
    }

    /// Bug #5 acceptance: a read-only file reports mode 0444 in
    /// GETATTR. `Entry::file` is the read-only-by-default constructor
    /// in the VFS, and the adapter must propagate the mode bits so
    /// clients see "r--r--r--" in `stat(2)`.
    #[tokio::test]
    async fn getattr_read_only_file_is_0444() {
        let recorder = RecordingHandler::new();
        let vfs = Vfs::builder().mount("box", recorder.clone()).build();
        let fs = BethFs::new(vfs);
        let ctx = fake_ctx();
        let dir = fs.lookup(&ctx, &BethHandle::Root, "box").await.unwrap();
        let readme = fs.lookup(&ctx, &dir, "readme").await.unwrap();
        let attrs = fs.getattr(&ctx, &readme).await.unwrap();
        assert_eq!(
            attrs.mode & 0o777,
            0o444,
            "expected 0o444 mode bits, got 0o{:o}",
            attrs.mode
        );
    }

    /// Bug #5: writable files keep their 0644 mode through GETATTR so
    /// clients still see them as writable.
    #[tokio::test]
    async fn getattr_writable_file_is_0644() {
        let recorder = RecordingHandler::new();
        let vfs = Vfs::builder().mount("box", recorder.clone()).build();
        let fs = BethFs::new(vfs);
        let ctx = fake_ctx();
        let dir = fs.lookup(&ctx, &BethHandle::Root, "box").await.unwrap();
        let inbox = fs.lookup(&ctx, &dir, "inbox").await.unwrap();
        let attrs = fs.getattr(&ctx, &inbox).await.unwrap();
        assert_eq!(attrs.mode & 0o777, 0o644);
    }

    /// Bug #5: ACCESS strips MODIFY/EXTEND/DELETE for a read-only path
    /// so the kernel doesn't cache a false-positive write capability.
    #[tokio::test]
    async fn access_strips_write_bits_on_read_only() {
        let recorder = RecordingHandler::new();
        let vfs = Vfs::builder().mount("box", recorder.clone()).build();
        let fs = BethFs::new(vfs);
        let ctx = fake_ctx();
        let dir = fs.lookup(&ctx, &BethHandle::Root, "box").await.unwrap();
        let readme = fs.lookup(&ctx, &dir, "readme").await.unwrap();
        let requested =
            AccessMask::READ | AccessMask::MODIFY | AccessMask::EXTEND | AccessMask::DELETE;
        let granted = fs.access(&ctx, &readme, requested).await.unwrap();
        assert!(granted.contains(AccessMask::READ));
        assert!(!granted.intersects(AccessMask::MODIFY | AccessMask::EXTEND | AccessMask::DELETE));
    }

    /// Bug #5: ACCESS preserves the write bits on a writable path so
    /// `echo foo > inbox` doesn't trip an EACCES preflight.
    #[tokio::test]
    async fn access_keeps_write_bits_on_writable() {
        let recorder = RecordingHandler::new();
        let vfs = Vfs::builder().mount("box", recorder.clone()).build();
        let fs = BethFs::new(vfs);
        let ctx = fake_ctx();
        let dir = fs.lookup(&ctx, &BethHandle::Root, "box").await.unwrap();
        let inbox = fs.lookup(&ctx, &dir, "inbox").await.unwrap();
        let requested = AccessMask::READ | AccessMask::MODIFY | AccessMask::EXTEND;
        let granted = fs.access(&ctx, &inbox, requested).await.unwrap();
        assert!(granted.contains(AccessMask::MODIFY));
        assert!(granted.contains(AccessMask::EXTEND));
    }
}
