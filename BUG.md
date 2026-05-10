# BUG-1: NFS file sizes report 8 MiB and reads pad with NULs

## Symptom

Every regular file under the beth NFS mount reports `st_size = 8388608`
regardless of its real content length. Tools that trust the stat'd size
(cat, cp, jq, head, most stdlib readers on macOS) read the real bytes
followed by NUL padding up to 8 MiB. Concretely:

- `jq /eth/status/backends/summary.json` prints valid JSON, then errors
  on trailing NULs.
- `cp /eth/tools/base64/decode/aGVsbG8= out` produces an 8 MiB output
  whose first 5 bytes are `hello` and the rest are zeros.
- `stat` on any leaf shows 8 MiB.

## Why the current design fails

`crates/beth-mount/src/adapter.rs:44` defines
`SYNTHETIC_READ_SIZE_HINT = 8 * 1024 * 1024`. `entry_to_attrs`
(`adapter.rs:154`) substitutes that value whenever a VFS `Entry` reports
`size == 0`:

```rust
let size = if e.kind == EntryKind::File && e.size == 0 {
    SYNTHETIC_READ_SIZE_HINT
} else {
    e.size
};
```

The sentinel exists because the Linux NFSv4 client treats `size == 0`
as EOF and may never issue READ. The 8 MiB lie forces READs to happen,
but at the cost of NUL-padding every short file to 8 MiB.

There is no constant placeholder size that fixes both problems on both
Linux and macOS NFS clients:

| Reported size | Failure mode |
|---|---|
| `0` | Linux/macOS may skip READ; `cat` returns nothing |
| Below real | Truncation; `jq` parses partial JSON |
| Above real | NUL padding past EOF; current bug |

NFS protocol requires the size returned at GETATTR to govern subsequent
READ. We must return the real size at GETATTR for files we want
useful reads on. There is no other correct option.

## Constraints the fix must respect

1. **Not every file is safely renderable at GETATTR time.** The mount
   surface mixes:
   - **Pure read-only data** (`chains/`, `prices/`, `status/`, `docs/`,
     most of `tools/`) — rendering on stat is fine.
   - **Write-only / control sinks** (`outbox/.../confirm`,
     `outbox/.../cancel`, `watch/new`, `defi/.../confirm`,
     `wallets/new`) — `read()` either errors or is meaningless. Calling
     `read()` from `getattr()` would either fail stat (if the handler
     returns `NotAFile`) or compute a useless body.
   - **Side-effecting reads** (`wallets/<w>/sign/<msg>` —
     `crates/beth-vfs/src/handlers/wallets.rs:110`). Reading a sign file
     produces a signature. Triggering that on every `stat` would be a
     real, dangerous footgun: directory listings, tab-completion,
     editor file-watchers, and shell prompt integrations all stat
     speculatively.

   The current implementation doesn't have this problem because
   `getattr()` (`adapter.rs:461`) only calls `vfs.lookup()`, never
   `vfs.read()`.

2. **`Handler::is_read_side_effecting` exists but no handler overrides
   it.** Defined at `crates/beth-vfs/src/handler.rs:156`, default
   `false`. Production code does not override this. Before we can rely
   on it as a gate, every side-effecting handler must be audited and
   updated. See the audit task below — this is mandatory and lands
   before the GETATTR render switch flips on.

3. **`PathCache` is opt-in.** `Handler::cache_ttl()` defaults to `None`
   (`handler.rs:146`); the router skips caching unless a handler
   returns `Some(ttl)`. Many handlers don't. We can't piggyback on
   `PathCache` for the GETATTR↔READ consistency bridge: a handler that
   doesn't opt in would render twice (once at GETATTR for size, once at
   READ for bytes), and dynamic content could differ between the two.

4. **Mount option assumptions don't transfer between Linux and macOS.**
   - Linux today (`crates/beth-mount/src/lib.rs:171`) sets
     `noac,lookupcache=none`. `noac` disables attribute caching;
     `lookupcache=none` disables dentry caching. These are independent
     and Linux-specific.
   - Apple's `mount_nfs` documents `actimeo=0` (and `noac` as an alias
     for it) for attribute caching. There is no `lookupcache=none`.
     `nocallback` is documented but unrelated to attribute correctness;
     `nonegnamecache` only affects negative caching.
   - `noac` controls **metadata** caching, not data caching. Nothing in
     the kernel mount options forces a fresh GETATTR before *every*
     read in all cases — particularly when READDIRPLUS has populated
     attributes during a parent listing.

## Fix

A two-part change: (a) make GETATTR conditionally render readable, pure
files; (b) keep writable / side-effecting / control files at size 0.

### (a) Conditional render in GETATTR

Add a mount-layer policy that decides per-path whether to render. The
policy uses information that is already available in `lookup()`:

```
GETATTR(path):
  e = vfs.lookup(path)              // current code; cheap, no render
  if e.kind != File                  → return dir/symlink attrs as today
  if not should_render_for_attrs(e) → return Attrs { size = 0, ... }
  bytes = render_with_dedup_and_timeout(path)
  mount_render_cache.put(path, bytes, ttl=750ms)
  return Attrs { size = bytes.len(), ... }

should_render_for_attrs(e):
  if e.mode & 0o200 != 0  → false   // writable file: skip; size=0 OK
  if vfs.is_read_side_effecting(path) → false
  return true
```

The mode-bit check is the conservative gate: any file the daemon
exposes as writable (mode 0o644) is skipped, even if it's also
nominally readable. This is a deliberate over-conservative cut. The
class of writable files is small (the v1-spec injection points listed
in `handler.rs:42-46`) and none of them are users' normal `cat`
targets — losing accurate size on those is acceptable. The benefit is
that a misclassified handler can't accidentally trigger a render via
GETATTR.

`is_read_side_effecting` is the second gate, for the rare case of a
read-only-mode file whose read still has side effects. Today no
handler overrides it; the audit (below) populates it before this fix
ships.

### (b) Mount-side render cache

A new struct on `BethFs`:

```rust
struct MountRenderCache {
    inner: Mutex<LruCache<VfsPath, MountRenderEntry>>,
}
struct MountRenderEntry {
    bytes: Bytes,
    expires_at: Instant,
}
```

Default capacity 1024, TTL 750 ms. Always-on, independent of
`Handler::cache_ttl`. Populated by GETATTR and consumed by the next
READ. On READ-without-prior-GETATTR (rare; client-implementation
specific), READ falls back to the normal VFS read path.

This is the GETATTR↔READ consistency bridge. It is intentionally not
the same as `PathCache`:

| | `PathCache` | `MountRenderCache` |
|---|---|---|
| Lives in | `beth-vfs` router | `beth-mount` adapter |
| TTL source | `Handler::cache_ttl` (opt-in, `None` default) | Constant 750 ms |
| Purpose | Reuse handler reads across operations | Bridge GETATTR result to READ |
| Invalidation | Write on top-level prefix | TTL-only |
| Capacity | 4096 entries, LRU | 1024 entries, LRU |

Both can coexist. PathCache may be a hit on the GETATTR-side render
for handlers that opt in (e.g. `chains/<c>/head/number` with 1 s TTL) —
in that case the render is even cheaper. PathCache being a miss is
the common case, and the mount-side cache picks up the slack.

### (c) Drop `SYNTHETIC_READ_SIZE_HINT`

Remove the constant and the conditional in `entry_to_attrs`. Files
that aren't rendered in GETATTR get `size = 0` and the original "Linux
NFS treats size=0 as EOF" issue would resurface for those — but for
writable control files that's the desired behavior (no one reads
`outbox/.../confirm`), and for side-effecting files we'd rather have
"cat returns empty" than "stat triggers a sign."

For the genuine "read-only file the handler reports size 0 for"
case — this is now the rendered path: GETATTR returns the actual
length.

### (d) Mount option corrections

`build_mount_args` in `lib.rs:168`:

- **Linux**: keep `noac,lookupcache=none,timeo=10`.
- **macOS** (new arm, `cfg(target_os = "macos")`): use
  `actimeo=0,timeo=10`. Drop `lookupcache=none` (Linux-only);
  do not add `nocallback`/`nonegnamecache` (unrelated to size
  correctness). The macOS `mount_nfs` man page documents `actimeo=0`
  and `noac` as the attribute-cache-disable knob.
- **Other Unix**: same as macOS (`actimeo=0`); the option is broadly
  portable.

A `target_os` `cfg` arm in the function — three branches, each
returning the right opts string.

### (e) Audit `is_read_side_effecting`

Mandatory before flipping the GETATTR render switch. Walk every handler
in `crates/beth-vfs/src/handlers/` and override
`is_read_side_effecting` to `true` for any path whose `read()`
mutates external state. Confirmed candidates from a first pass:

- `wallets/<w>/sign/<message>` — produces signature. **Must** be
  flagged; reading triggers signing.
- `wallets/<w>/sign/typed` — same.
- `wallets/<w>/chains/<c>/outbox/.../confirm` — broadcast on read?
  Verify in `handlers/wallets.rs:489`. The current code looks like
  this is reached via `write()`, not `read()`, but the path-shape
  registration in `lookup()` could expose `read()`. Confirm by
  unit test.
- Any other handler with `signer.sign_*`, `client.send_raw_*`,
  `broadcast`, `commit`, `apply` in its `read()` path.

Output of audit: a per-handler test that calls `read()` on the
side-effecting paths and asserts the side effect fires. Then add
the override and re-run — assertion now fails (read returned an
error or a cached result without the side effect), and a
`getattr` test confirms we no longer call into the dangerous code.

### (f) In-flight render dedup

Concurrent GETATTRs on the same path share one render future. NFS
clients retry slow ops; a cold `chains/<c>/tx/<h>/error.json` is one
render that can take 30 s and we cannot allow that to stampede.

```rust
in_flight: Arc<Mutex<HashMap<VfsPath, Shared<BoxFuture<Result<Bytes, HandlerError>>>>>>
```

`futures::FutureExt::shared` covers many-awaiters / one-producer.
Entry is removed from the map when the future resolves so subsequent
requests after TTL can re-render.

### (g) Server-side render timeout

Wrap renders in `tokio::time::timeout` with a default of 30 s. On
timeout map to `FsError::Io` so the client sees EIO with a logged
reason rather than hanging past the kernel's `timeo` threshold (Linux
mount uses `timeo=10` deciseconds = 1 s with retry escalation; we are
well below the eventual hard ceiling).

## Files to change

| File | Change |
|---|---|
| `crates/beth-mount/src/adapter.rs` | Remove `SYNTHETIC_READ_SIZE_HINT`. Rewrite `entry_to_attrs` to take an explicit `size: u64`. Rewrite `getattr` for files: gate on mode + `is_read_side_effecting`, render via VFS, populate `MountRenderCache`. Add `MountRenderCache`. Add in-flight dedup map. Add render timeout. Make `read()` consult `MountRenderCache` first. |
| `crates/beth-mount/src/lib.rs` | Split `build_mount_args` into `target_os` arms: Linux (current opts), macOS (`actimeo=0,timeo=10,...`), other Unix (same as macOS). Add unit tests for each arm. |
| `crates/beth-vfs/src/lib.rs` | Expose `Vfs::is_read_side_effecting(path) -> bool` so the mount layer can consult it without owning a `Handler` reference. (Wraps `Router`'s existing call site at `router.rs:168`.) |
| `crates/beth-vfs/src/handlers/wallets.rs` | Override `is_read_side_effecting` to return `true` for sign paths, broadcast paths, and any other read-with-side-effect leaf. |
| `crates/beth-vfs/src/handlers/*.rs` | Audit-driven overrides of `is_read_side_effecting`. Per-handler unit test that the predicate is `true` for each side-effecting path and `false` for the rest of that handler's surface. |
| `crates/beth-mount/tests/` | New integration tests (see Verification). |

## Costs we accept

1. **First `stat` of a cold expensive leaf is slow.** Intrinsic to
   NFS — there is no protocol path that lets size be "known later."
   Mitigated by handler-internal caches (revert decoder already caches,
   mem 1468) and the mount-layer dedup so retries and concurrent stats
   coalesce.
2. **`ls -l` on a directory of dynamic children is slow** if the
   children are pure read-only files (each child renders on its own
   GETATTR). For directories whose children are mostly writable
   controls or side-effecting (e.g. `wallets/<w>/sign/`) the listing
   stays cheap — those children report size 0.
3. **`actimeo=0` adds a GETATTR round-trip per read.** Negligible on
   localhost NFS; we already pay this on Linux today.
4. **Writable / side-effecting files report `size = 0`.** `cat` on
   `outbox/.../confirm` returns nothing. This is fine — those files
   exist for `echo > path` semantics, not reading.

## Out of scope

- Cache warming on directory enumeration. Defer until `ls -l` latency
  on heavy dirs is a real complaint.
- Per-handler cost classification (`cheap | expensive`). Rendering on
  demand at GETATTR, with dedup + timeout, covers the cases we have
  today.
- Reworking `change` attribute to be content-hash-based. Current
  nanosecond-fresh behavior over-invalidates but isn't wrong with
  `noac`.
- Forcing kernel READDIRPLUS off (`nordirplus` on Linux, no equivalent
  on macOS). Address only if testing shows readdirplus-cached zero
  sizes leak into reads despite `actimeo=0`.

## Verification plan

Linux first, macOS gated on Linux passing.

### Unit / adapter tests

1. `getattr_renders_pure_read_only_file_returns_real_size` — register
   a handler with a fixed-bytes `read()`, mode 0o444, no side effect.
   Assert `getattr(...).size == bytes.len()`.
2. `getattr_skips_render_for_writable_file` — handler with mode 0o644,
   `read()` returns a body. Assert `getattr(...).size == 0` and the
   handler's `read()` was *not* called.
3. `getattr_skips_render_for_side_effecting_file` — handler with mode
   0o444, `is_read_side_effecting` returns `true`, `read()` increments
   a counter. Assert counter unchanged after `getattr`, and
   `getattr(...).size == 0`.
4. `getattr_then_read_returns_same_bytes_no_padding` — render counter
   on the handler. After `getattr`, immediate `read` returns identical
   bytes; counter incremented exactly once across the pair.
5. `concurrent_getattrs_dedup_to_one_render` — 32 parallel `getattr`
   calls on the same cold path. Render counter incremented once.
6. `render_timeout_returns_eio` — handler `read()` sleeps past timeout.
   Assert `FsError::Io` returned.

### Mount-level smoke (Linux)

Run a real mount, exercise:

- `stat /mnt/eth/status/daemon.json` → size matches `wc -c` of that
  content rendered via `bethctl` direct API.
- `jq . /mnt/eth/status/backends/summary.json` → exits 0, no trailing
  garbage.
- `cp /mnt/eth/tools/base64/decode/aGVsbG8= /tmp/out && wc -c /tmp/out`
  → 5.
- `ls -la /mnt/eth/status/` → no entry shows 8 MiB; readable files
  show real size, writable shows 0.
- `cat /mnt/eth/wallets/<w>/sign/hello` → must NOT trigger a sign just
  from a prior `stat`. Sign happens only on the explicit read.

### Mount-level smoke (macOS)

Same suite. Specifically:

- After the suite, check `defaults read /Library/Preferences/...` is
  not relevant — these are mount opts, and the daemon process
  inspects them via `mount` command output. Use
  `mount | grep <mountpoint>` to confirm `actimeo=0` made it through.
- READDIRPLUS leakage check: `ls -la /mnt/eth/status/` then
  `cat /mnt/eth/status/daemon.json` immediately after. Assert content
  matches expected and is not truncated/empty/padded. If macOS leaks
  zero sizes from rdirplus → consider rendering for readable
  dir-children at READDIRPLUS time as a follow-up, or document a
  required `nordirplus`-equivalent.

### Existing tests

The `getattr_*` tests in `adapter.rs:1163+` need updating — assertions
about size are wrong post-fix. Fix as part of the same change.
