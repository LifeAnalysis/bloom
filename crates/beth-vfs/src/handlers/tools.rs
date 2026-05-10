//! `tools/` — pure helpers. Two interaction patterns:
//!
//! Stateless one-shots (read returns the result of the operation):
//! - `tools/keccak/<input>`
//! - `tools/sha256/<input>` and `tools/blake3/<input>`
//! - `tools/selector/<sig>`
//! - `tools/address/checksum/<addr>` (EIP-55 form)
//! - `tools/unit/parse/<value>/<unit>`
//! - `tools/unit/format/<wei>/<decimals>`
//! - `tools/hex/encode/<utf8>` and `tools/hex/decode/<hex>`
//! - `tools/base64/encode/<utf8>` and `tools/base64/decode/<b64>`
//!
//! Stateful write-then-read sessions (for inputs that can't safely be
//! crammed into a path):
//! - `tools/abi/encode/<session>/in.json` (write) + `out.hex` (read)
//! - `tools/abi/decode/<session>/in.json` (write) + `out.json` (read)
//! - `tools/eip712/hash/<session>/in.json` (write) + `out.hex` (read)
//! - `tools/rlp/encode/<session>/in.json` (write) + `out.hex` (read)
//! - `tools/rlp/decode/<session>/in.json` (write) + `out.json` (read)
//!
//! Sessions auto-expire after 5 minutes of idle.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;

use crate::handler::{Entry, EntryKind, Handler, HandlerError};
use crate::path::VfsPath;

const SESSION_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Debug)]
struct ToolsSession {
    /// The bytes the user wrote to `in.json`.
    input: Vec<u8>,
    /// Idle deadline; refreshed on each touch.
    expires: Instant,
}

#[derive(Default)]
struct SessionStore {
    /// Keyed by `(family, session_id)`, where `family` is e.g.
    /// `"abi/encode"`, `"eip712/hash"`, `"rlp/decode"`, etc.
    inner: HashMap<(String, String), ToolsSession>,
}

impl SessionStore {
    fn purge_expired(&mut self, now: Instant) {
        self.inner.retain(|_, s| s.expires > now);
    }

    fn put(&mut self, family: &str, sid: &str, input: Vec<u8>) {
        let now = Instant::now();
        self.purge_expired(now);
        self.inner.insert(
            (family.to_string(), sid.to_string()),
            ToolsSession {
                input,
                expires: now + SESSION_TTL,
            },
        );
    }

    fn get(&mut self, family: &str, sid: &str) -> Option<Vec<u8>> {
        let now = Instant::now();
        self.purge_expired(now);
        let entry = self.inner.get_mut(&(family.to_string(), sid.to_string()))?;
        entry.expires = now + SESSION_TTL;
        Some(entry.input.clone())
    }
}

#[derive(Clone, Default)]
pub struct ToolsHandler {
    sessions: Arc<Mutex<SessionStore>>,
}

impl ToolsHandler {
    pub fn new() -> Self {
        Self::default()
    }
}

const TOOLS_TOP: &[&str] = &[
    "keccak", "selector", "address", "unit", "sha256", "blake3", "hex", "base64", "abi", "eip712",
    "rlp",
];

/// Session-bearing families, keyed as `<family>/<session>/{in.json,out.*}`.
/// The path within the family is `<a>/<b>/...` joined by `/`. The output
/// file name is the read-side terminator.
const SESSION_FAMILIES: &[(&str, &[&str], &str)] = &[
    ("abi/encode", &["abi", "encode"], "out.hex"),
    ("abi/decode", &["abi", "decode"], "out.json"),
    ("eip712/hash", &["eip712", "hash"], "out.hex"),
    ("rlp/encode", &["rlp", "encode"], "out.hex"),
    ("rlp/decode", &["rlp", "decode"], "out.json"),
];

fn match_session_family(segs: &[String]) -> Option<(&'static str, &[String])> {
    for (family, prefix, _out) in SESSION_FAMILIES {
        if segs.len() >= prefix.len()
            && segs[..prefix.len()]
                .iter()
                .zip(prefix.iter())
                .all(|(a, b)| a.as_str() == *b)
        {
            return Some((family, &segs[prefix.len()..]));
        }
    }
    None
}

fn family_out_name(family: &str) -> &'static str {
    SESSION_FAMILIES
        .iter()
        .find(|(f, _, _)| *f == family)
        .map(|(_, _, o)| *o)
        .unwrap_or("out.bin")
}

#[async_trait]
impl Handler for ToolsHandler {
    async fn lookup(&self, path: &VfsPath) -> Result<Entry, HandlerError> {
        let r = self.lookup_inner(path).await;
        if let Err(e) = &r {
            tracing::debug!(path = %path.to_string_path(), error = %e, "tools.lookup_err");
        }
        r
    }

    async fn read(&self, path: &VfsPath) -> Result<Vec<u8>, HandlerError> {
        let r = self.read_inner(path).await;
        if let Err(e) = &r {
            tracing::debug!(path = %path.to_string_path(), error = %e, "tools.read_err");
        }
        r
    }

    async fn write(&self, path: &VfsPath, data: &[u8]) -> Result<(), HandlerError> {
        let r = self.write_inner(path, data).await;
        if let Err(e) = &r {
            tracing::debug!(
                path = %path.to_string_path(),
                bytes = data.len(),
                error = %e,
                "tools.write_err"
            );
        }
        r
    }

    async fn list(&self, path: &VfsPath) -> Result<Vec<Entry>, HandlerError> {
        let r = self.list_inner(path).await;
        if let Err(e) = &r {
            tracing::debug!(path = %path.to_string_path(), error = %e, "tools.list_err");
        }
        r
    }
}

impl ToolsHandler {
    async fn lookup_inner(&self, path: &VfsPath) -> Result<Entry, HandlerError> {
        if path.is_root() {
            return Ok(Entry::dir(""));
        }
        let segs = path.segments();
        // Session families take precedence so that `abi/encode/<session>` is
        // handled here before falling through to the open-ended-dir branch.
        if let Some((family, rest)) = match_session_family(segs) {
            let out = family_out_name(family);
            return match rest.len() {
                0 => Ok(Entry::dir(segs.last().unwrap().as_str())),
                1 => Ok(Entry::dir(&rest[0])), // session dir
                2 => {
                    let last = rest[1].as_str();
                    if last == "in.json" {
                        Ok(Entry::writable_file("in.json"))
                    } else if last == out {
                        Ok(Entry::file(out))
                    } else {
                        Err(HandlerError::not_found(path.to_string_path()))
                    }
                }
                _ => Err(HandlerError::not_found(path.to_string_path())),
            };
        }
        match segs[0].as_str() {
            "keccak" | "sha256" | "blake3" | "selector" => {
                if segs.len() == 1 {
                    Ok(Entry::dir(&segs[0]))
                } else {
                    Ok(Entry::file(segs.last().unwrap()))
                }
            }
            "address" => {
                if segs.len() == 1 {
                    Ok(Entry::dir("address"))
                } else if segs.len() >= 2 && segs[1] == "checksum" {
                    if segs.len() == 2 {
                        Ok(Entry::dir("checksum"))
                    } else {
                        Ok(Entry::file(segs.last().unwrap()))
                    }
                } else {
                    Err(HandlerError::not_found(path.to_string_path()))
                }
            }
            "unit" => {
                // /unit                           -> dir
                // /unit/parse | /unit/format      -> dir
                // /unit/{parse,format}/<value>    -> dir (the value is a
                //                                   directory whose children
                //                                   are the unit / decimals)
                // /unit/{parse,format}/<value>/<u>-> file (computes result)
                if segs.len() == 1 {
                    Ok(Entry::dir("unit"))
                } else if segs[1] == "parse" || segs[1] == "format" {
                    match segs.len() {
                        2 => Ok(Entry::dir(&segs[1])),
                        3 => Ok(Entry::dir(&segs[2])),
                        4 => Ok(Entry::file(segs.last().unwrap())),
                        _ => Err(HandlerError::not_found(path.to_string_path())),
                    }
                } else {
                    Err(HandlerError::not_found(path.to_string_path()))
                }
            }
            "hex" | "base64" => {
                if segs.len() == 1 {
                    Ok(Entry::dir(&segs[0]))
                } else if segs[1] == "encode" || segs[1] == "decode" {
                    if segs.len() == 2 {
                        Ok(Entry::dir(&segs[1]))
                    } else {
                        Ok(Entry::file(segs.last().unwrap()))
                    }
                } else {
                    Err(HandlerError::not_found(path.to_string_path()))
                }
            }
            "abi" | "eip712" | "rlp" => {
                // Family root; we already handled deeper paths above.
                if segs.len() == 1 {
                    Ok(Entry::dir(&segs[0]))
                } else {
                    Err(HandlerError::not_found(path.to_string_path()))
                }
            }
            _ => Err(HandlerError::not_found(path.to_string_path())),
        }
    }

    async fn read_inner(&self, path: &VfsPath) -> Result<Vec<u8>, HandlerError> {
        let segs = path.segments();
        if segs.is_empty() {
            return Err(HandlerError::NotAFile(path.to_string_path()));
        }

        // Session-family reads of `out.*`.
        if let Some((family, rest)) = match_session_family(segs) {
            let out_name = family_out_name(family);
            if rest.len() == 2 && rest[1].as_str() == out_name {
                let session_id = rest[0].as_str();
                let input = {
                    let mut store = self.sessions.lock();
                    store.get(family, session_id).ok_or_else(|| {
                        HandlerError::not_found(format!(
                            "session '{}' not found (write {} first)",
                            session_id, "in.json"
                        ))
                    })?
                };
                let bytes = compute_session_output(family, &input)?;
                return Ok(bytes);
            }
            // `in.json` read returns whatever was written (best effort).
            if rest.len() == 2 && rest[1].as_str() == "in.json" {
                let session_id = rest[0].as_str();
                let mut store = self.sessions.lock();
                if let Some(input) = store.get(family, session_id) {
                    return Ok(input);
                }
                return Err(HandlerError::not_found(path.to_string_path()));
            }
            return Err(HandlerError::NotAFile(path.to_string_path()));
        }

        match segs[0].as_str() {
            "keccak" if segs.len() >= 2 => {
                let input = segs[1..].join("/");
                let h = beth_tools::keccak_hex(input.as_bytes());
                Ok(format!("{}\n", h).into_bytes())
            }
            "sha256" if segs.len() >= 2 => {
                let input = segs[1..].join("/");
                let h = beth_tools::sha256_hex(input.as_bytes());
                Ok(format!("{}\n", h).into_bytes())
            }
            "blake3" if segs.len() >= 2 => {
                let input = segs[1..].join("/");
                let h = beth_tools::blake3_hex(input.as_bytes());
                Ok(format!("{}\n", h).into_bytes())
            }
            "selector" if segs.len() >= 2 => {
                let sig = segs[1..].join("/");
                let s = beth_tools::selector(&sig);
                Ok(format!("{}\n", s).into_bytes())
            }
            "address" if segs.len() >= 3 && segs[1] == "checksum" => {
                let addr = &segs[2];
                let cs =
                    beth_tools::checksum(addr).map_err(|e| HandlerError::invalid(e.to_string()))?;
                Ok(format!("{}\n", cs).into_bytes())
            }
            "unit" if segs.len() >= 4 && segs[1] == "parse" => {
                // /unit/parse/<value>/<unit>
                let value = &segs[2];
                let unit = &segs[3];
                let combined = format!("{} {}", value, unit);
                let parsed = beth_proto::parse_amount(&combined)
                    .map_err(|e| HandlerError::invalid(e.to_string()))?;
                let raw = parsed.raw.ok_or_else(|| {
                    HandlerError::invalid(format!(
                        "non-native unit '{}' — resolve via token decimals",
                        parsed.unit
                    ))
                })?;
                Ok(format!("{}\n", raw).into_bytes())
            }
            "unit" if segs.len() >= 4 && segs[1] == "format" => {
                // /unit/format/<wei>/<decimals>
                let wei: alloy::primitives::U256 = segs[2]
                    .parse()
                    .map_err(|_| HandlerError::invalid("not a u256"))?;
                let decimals: u8 = segs[3]
                    .parse()
                    .map_err(|_| HandlerError::invalid("not a u8"))?;
                Ok(format!("{}\n", beth_proto::format_units(wei, decimals)).into_bytes())
            }
            "hex" if segs.len() >= 3 && segs[1] == "encode" => {
                let input = segs[2..].join("/");
                Ok(format!("{}\n", beth_tools::hex_encode(input.as_bytes())).into_bytes())
            }
            "hex" if segs.len() >= 3 && segs[1] == "decode" => {
                let input = segs[2..].join("/");
                let bytes = beth_tools::hex_decode(&input)
                    .map_err(|e| HandlerError::invalid(e.to_string()))?;
                // Return raw decoded bytes — caller may interpret them.
                Ok(bytes)
            }
            "base64" if segs.len() >= 3 && segs[1] == "encode" => {
                let input = segs[2..].join("/");
                Ok(format!("{}\n", beth_tools::base64_encode(input.as_bytes())).into_bytes())
            }
            "base64" if segs.len() >= 3 && segs[1] == "decode" => {
                let input = segs[2..].join("/");
                let bytes = beth_tools::base64_decode(&input)
                    .map_err(|e| HandlerError::invalid(e.to_string()))?;
                Ok(bytes)
            }
            _ => Err(HandlerError::NotAFile(path.to_string_path())),
        }
    }

    async fn write_inner(&self, path: &VfsPath, data: &[u8]) -> Result<(), HandlerError> {
        let segs = path.segments();
        if let Some((family, rest)) = match_session_family(segs) {
            if rest.len() == 2 && rest[1].as_str() == "in.json" {
                // Eagerly validate the JSON shape so writes fail fast; the
                // actual computation still happens at read time.
                serde_json::from_slice::<serde_json::Value>(data)
                    .map_err(|e| HandlerError::invalid(format!("invalid json: {}", e)))?;
                let session_id = rest[0].as_str();
                self.sessions.lock().put(family, session_id, data.to_vec());
                return Ok(());
            }
        }
        let _ = path;
        Err(HandlerError::PermissionDenied)
    }

    async fn list_inner(&self, path: &VfsPath) -> Result<Vec<Entry>, HandlerError> {
        if path.is_root() {
            return Ok(TOOLS_TOP.iter().map(|s| Entry::dir(s)).collect());
        }
        let segs = path.segments();

        // Session-family listing: family root lists known sessions; session
        // dir lists `in.json` + the family's output file.
        if let Some((family, rest)) = match_session_family(segs) {
            return match rest.len() {
                0 => {
                    // List all live sessions for this family.
                    let mut store = self.sessions.lock();
                    store.purge_expired(Instant::now());
                    Ok(store
                        .inner
                        .keys()
                        .filter(|(f, _)| f == family)
                        .map(|(_, sid)| Entry::dir(sid))
                        .collect())
                }
                1 => {
                    // Session dir: present in.json + output, regardless of
                    // whether the user has written yet, so an `ls` is helpful.
                    let out = family_out_name(family);
                    Ok(vec![Entry::writable_file("in.json"), Entry::file(out)])
                }
                _ => Ok(Vec::new()),
            };
        }

        match (segs[0].as_str(), segs.len()) {
            ("address", 1) => Ok(vec![Entry::dir("checksum")]),
            ("unit", 1) => Ok(vec![Entry::dir("parse"), Entry::dir("format")]),
            ("hex", 1) | ("base64", 1) => Ok(vec![Entry::dir("encode"), Entry::dir("decode")]),
            ("abi", 1) => Ok(vec![Entry::dir("encode"), Entry::dir("decode")]),
            ("eip712", 1) => Ok(vec![Entry::dir("hash")]),
            ("rlp", 1) => Ok(vec![Entry::dir("encode"), Entry::dir("decode")]),
            // Other dirs are open-ended (any segment is a synthetic file)
            _ => Ok(Vec::new()),
        }
    }
}

/// Run the actual computation given a session family + raw input bytes.
fn compute_session_output(family: &str, input: &[u8]) -> Result<Vec<u8>, HandlerError> {
    let json: serde_json::Value = serde_json::from_slice(input)
        .map_err(|e| HandlerError::invalid(format!("invalid json: {}", e)))?;
    match family {
        "abi/encode" => {
            // Expect { "sig": "...", "args": [...] }.
            let sig = json
                .get("sig")
                .and_then(|v| v.as_str())
                .ok_or_else(|| HandlerError::invalid("missing 'sig' string"))?;
            let args = json
                .get("args")
                .ok_or_else(|| HandlerError::invalid("missing 'args' array"))?;
            let calldata = beth_tools::abi_encode(sig, args)
                .map_err(|e| HandlerError::invalid(e.to_string()))?;
            Ok(format!("{}\n", calldata).into_bytes())
        }
        "abi/decode" => {
            // Expect { "types": ["uint256", ...], "data": "0x..." }.
            let types_json = json
                .get("types")
                .and_then(|v| v.as_array())
                .ok_or_else(|| HandlerError::invalid("missing 'types' array"))?;
            let types: Vec<&str> = types_json
                .iter()
                .map(|v| {
                    v.as_str()
                        .ok_or_else(|| HandlerError::invalid("type must be a string"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let data_hex = json
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or_else(|| HandlerError::invalid("missing 'data' hex string"))?;
            let data = beth_tools::hex_decode(data_hex)
                .map_err(|e| HandlerError::invalid(e.to_string()))?;
            let decoded = beth_tools::abi_decode(&types, &data)
                .map_err(|e| HandlerError::invalid(e.to_string()))?;
            let mut out = serde_json::to_vec_pretty(&decoded)
                .map_err(|e| HandlerError::invalid(e.to_string()))?;
            out.push(b'\n');
            Ok(out)
        }
        "eip712/hash" => {
            // The whole input IS the typed-data document.
            let s = std::str::from_utf8(input)
                .map_err(|e| HandlerError::invalid(format!("input not utf-8: {}", e)))?;
            let h = beth_tools::eip712_hash(s).map_err(|e| HandlerError::invalid(e.to_string()))?;
            Ok(format!("{}\n", h).into_bytes())
        }
        "rlp/encode" => {
            // Expect { "value": <json-tree> } or the bare value.
            let value = json.get("value").unwrap_or(&json);
            let h =
                beth_tools::rlp_encode(value).map_err(|e| HandlerError::invalid(e.to_string()))?;
            Ok(format!("{}\n", h).into_bytes())
        }
        "rlp/decode" => {
            // Expect { "data": "0x..." }.
            let data_hex = json
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or_else(|| HandlerError::invalid("missing 'data' hex string"))?;
            let data = beth_tools::hex_decode(data_hex)
                .map_err(|e| HandlerError::invalid(e.to_string()))?;
            let decoded =
                beth_tools::rlp_decode(&data).map_err(|e| HandlerError::invalid(e.to_string()))?;
            let mut out = serde_json::to_vec_pretty(&decoded)
                .map_err(|e| HandlerError::invalid(e.to_string()))?;
            out.push(b'\n');
            Ok(out)
        }
        other => Err(HandlerError::invalid(format!(
            "unknown session family: {}",
            other
        ))),
    }
}

// Avoid unused warnings.
const _ENTRY: fn(&Entry) -> EntryKind = |e: &Entry| e.kind;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keccak_one_shot() {
        let h = ToolsHandler::new();
        let p = VfsPath::parse("/keccak/abc").unwrap();
        let v = h.read(&p).await.unwrap();
        let s = String::from_utf8(v).unwrap();
        assert!(s.starts_with("0x"));
        assert!(s.trim().len() == 66);
    }

    #[tokio::test]
    async fn sha256_one_shot() {
        let h = ToolsHandler::new();
        let p = VfsPath::parse("/sha256/abc").unwrap();
        let v = h.read(&p).await.unwrap();
        let s = String::from_utf8(v).unwrap();
        assert_eq!(
            s.trim(),
            "0xba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[tokio::test]
    async fn blake3_one_shot() {
        let h = ToolsHandler::new();
        let p = VfsPath::parse("/blake3/abc").unwrap();
        let v = h.read(&p).await.unwrap();
        let s = String::from_utf8(v).unwrap();
        assert!(s.starts_with("0x"));
        assert_eq!(s.trim().len(), 66);
    }

    #[tokio::test]
    async fn unit_parse_eth() {
        let h = ToolsHandler::new();
        let p = VfsPath::parse("/unit/parse/1.5/eth").unwrap();
        let v = h.read(&p).await.unwrap();
        let s = String::from_utf8(v).unwrap();
        assert_eq!(s.trim(), "1500000000000000000");
    }

    /// Regression: each component of `/unit/{parse,format}/<value>/<unit>`
    /// must lookup as a directory until the leaf, otherwise NFS clients
    /// see "Not a directory" when they walk the path one segment at a
    /// time. Bug where the `<value>` segment was incorrectly typed as a
    /// file.
    #[tokio::test]
    async fn unit_parse_intermediate_lookups_are_dirs() {
        let h = ToolsHandler::new();
        // Walk: unit -> parse -> 1.5 -> eth(file)
        let dir1 = h.lookup(&VfsPath::parse("/unit").unwrap()).await.unwrap();
        assert_eq!(dir1.kind, EntryKind::Dir);
        let dir2 = h
            .lookup(&VfsPath::parse("/unit/parse").unwrap())
            .await
            .unwrap();
        assert_eq!(dir2.kind, EntryKind::Dir);
        let dir3 = h
            .lookup(&VfsPath::parse("/unit/parse/1.5").unwrap())
            .await
            .unwrap();
        assert_eq!(dir3.kind, EntryKind::Dir, "<value> must be a directory");
        let leaf = h
            .lookup(&VfsPath::parse("/unit/parse/1.5/eth").unwrap())
            .await
            .unwrap();
        assert_eq!(leaf.kind, EntryKind::File);
    }

    #[tokio::test]
    async fn unit_format_intermediate_lookups_are_dirs() {
        let h = ToolsHandler::new();
        let dir = h
            .lookup(&VfsPath::parse("/unit/format/1500000000000000000").unwrap())
            .await
            .unwrap();
        assert_eq!(dir.kind, EntryKind::Dir, "<wei> must be a directory");
        let leaf = h
            .lookup(&VfsPath::parse("/unit/format/1500000000000000000/18").unwrap())
            .await
            .unwrap();
        assert_eq!(leaf.kind, EntryKind::File);
    }

    #[tokio::test]
    async fn unit_parse_integer_input() {
        let h = ToolsHandler::new();
        let p = VfsPath::parse("/unit/parse/25/gwei").unwrap();
        let v = h.read(&p).await.unwrap();
        let s = String::from_utf8(v).unwrap();
        assert_eq!(s.trim(), "25000000000");
    }

    #[tokio::test]
    async fn unit_parse_small_fraction() {
        let h = ToolsHandler::new();
        // Smallest representable eth value (1 wei).
        let p = VfsPath::parse("/unit/parse/0.000000000000000001/eth").unwrap();
        let v = h.read(&p).await.unwrap();
        let s = String::from_utf8(v).unwrap();
        assert_eq!(s.trim(), "1");
    }

    #[tokio::test]
    async fn unit_parse_ether_alias() {
        let h = ToolsHandler::new();
        // "ether" is an accepted alias for "eth".
        let p = VfsPath::parse("/unit/parse/2/ether").unwrap();
        let v = h.read(&p).await.unwrap();
        let s = String::from_utf8(v).unwrap();
        assert_eq!(s.trim(), "2000000000000000000");
    }

    #[tokio::test]
    async fn unit_format_various_decimals() {
        let h = ToolsHandler::new();
        // 6 decimals -> USDC-style (1_000_000 = 1.0).
        let p = VfsPath::parse("/unit/format/1000000/6").unwrap();
        let v = h.read(&p).await.unwrap();
        assert_eq!(String::from_utf8(v).unwrap().trim(), "1");

        // 9 decimals -> gwei (1_000_000_000 = 1.0).
        let p = VfsPath::parse("/unit/format/1000000000/9").unwrap();
        let v = h.read(&p).await.unwrap();
        assert_eq!(String::from_utf8(v).unwrap().trim(), "1");

        // 0 decimals -> identity.
        let p = VfsPath::parse("/unit/format/42/0").unwrap();
        let v = h.read(&p).await.unwrap();
        assert_eq!(String::from_utf8(v).unwrap().trim(), "42");
    }

    #[tokio::test]
    async fn unit_parse_invalid_value_is_invalid_not_not_found() {
        let h = ToolsHandler::new();
        let p = VfsPath::parse("/unit/parse/notanumber/eth").unwrap();
        let err = h.read(&p).await.unwrap_err();
        match err {
            HandlerError::Invalid(_) => {}
            other => panic!("expected Invalid, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn unit_parse_invalid_unit_is_invalid_not_not_found() {
        let h = ToolsHandler::new();
        let p = VfsPath::parse("/unit/parse/1.5/notaunit").unwrap();
        let err = h.read(&p).await.unwrap_err();
        match err {
            HandlerError::Invalid(_) => {}
            other => panic!("expected Invalid, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn unit_format_invalid_decimals_is_invalid() {
        let h = ToolsHandler::new();
        let p = VfsPath::parse("/unit/format/1000000000000000000/notadigit").unwrap();
        let err = h.read(&p).await.unwrap_err();
        match err {
            HandlerError::Invalid(_) => {}
            other => panic!("expected Invalid, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn unit_unknown_subcommand_is_not_found() {
        let h = ToolsHandler::new();
        let r = h.lookup(&VfsPath::parse("/unit/bogus").unwrap()).await;
        match r {
            Err(HandlerError::NotFound(_)) => {}
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn lists_top_level() {
        let h = ToolsHandler::new();
        let entries = h.list(&VfsPath::root()).await.unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        for expected in [
            "keccak", "selector", "address", "unit", "sha256", "blake3", "hex", "base64", "abi",
            "eip712", "rlp",
        ] {
            assert!(names.contains(&expected), "missing top-level {}", expected);
        }
    }

    #[tokio::test]
    async fn unit_format_wei() {
        let h = ToolsHandler::new();
        let p = VfsPath::parse("/unit/format/1500000000000000000/18").unwrap();
        let v = h.read(&p).await.unwrap();
        let s = String::from_utf8(v).unwrap();
        assert_eq!(s.trim(), "1.5");
    }

    #[tokio::test]
    async fn hex_encode_decode_one_shot() {
        let h = ToolsHandler::new();
        let p = VfsPath::parse("/hex/encode/hello").unwrap();
        let v = h.read(&p).await.unwrap();
        let s = String::from_utf8(v).unwrap();
        assert_eq!(s.trim(), "0x68656c6c6f");

        let p = VfsPath::parse("/hex/decode/0x68656c6c6f").unwrap();
        let v = h.read(&p).await.unwrap();
        assert_eq!(v, b"hello");
    }

    #[tokio::test]
    async fn base64_encode_decode_one_shot() {
        let h = ToolsHandler::new();
        let p = VfsPath::parse("/base64/encode/hello").unwrap();
        let v = h.read(&p).await.unwrap();
        let s = String::from_utf8(v).unwrap();
        assert_eq!(s.trim(), "aGVsbG8=");

        let p = VfsPath::parse("/base64/decode/aGVsbG8=").unwrap();
        let v = h.read(&p).await.unwrap();
        assert_eq!(v, b"hello");
    }

    #[tokio::test]
    async fn abi_encode_session_round_trip() {
        let h = ToolsHandler::new();
        let in_path = VfsPath::parse("/abi/encode/sess1/in.json").unwrap();
        let body = serde_json::json!({
            "sig": "transfer(address,uint256)",
            "args": ["0xd8da6bf26964af9d7eed9e03e53415d37aa96045", "1000000"]
        })
        .to_string();
        h.write(&in_path, body.as_bytes()).await.unwrap();
        let out_path = VfsPath::parse("/abi/encode/sess1/out.hex").unwrap();
        let v = h.read(&out_path).await.unwrap();
        let s = String::from_utf8(v).unwrap();
        // Selector for transfer(address,uint256).
        assert!(s.starts_with("0xa9059cbb"));
        // 4 bytes selector + 32 bytes addr + 32 bytes amount = 68 -> 136 hex chars + 0x.
        assert_eq!(s.trim().len(), 138);
    }

    #[tokio::test]
    async fn eip712_hash_session() {
        let h = ToolsHandler::new();
        let in_path = VfsPath::parse("/eip712/hash/m1/in.json").unwrap();
        let body = serde_json::json!({
            "domain": {},
            "types": {
                "EIP712Domain": [],
                "Person": [
                    {"name": "name", "type": "string"},
                    {"name": "wallet", "type": "address"}
                ],
                "Mail": [
                    {"name": "from", "type": "Person"},
                    {"name": "to", "type": "Person"},
                    {"name": "contents", "type": "string"}
                ]
            },
            "primaryType": "Mail",
            "message": {
                "from": {"name": "Cow", "wallet": "0xCD2a3d9F938E13CD947Ec05AbC7FE734Df8DD826"},
                "to":   {"name": "Bob", "wallet": "0xbBbBBBBbbBBBbbbBbbBbbbbBBbBbbbbBbBbbBBbB"},
                "contents": "Hello, Bob!"
            }
        })
        .to_string();
        h.write(&in_path, body.as_bytes()).await.unwrap();
        let out_path = VfsPath::parse("/eip712/hash/m1/out.hex").unwrap();
        let v = h.read(&out_path).await.unwrap();
        let s = String::from_utf8(v).unwrap();
        assert_eq!(
            s.trim(),
            "0x25c3d40a39e639a4d0b6e4d2ace5e1281e039c88494d97d8d08f99a6ea75d775"
        );
    }

    #[tokio::test]
    async fn rlp_encode_decode_session() {
        let h = ToolsHandler::new();
        let in_path = VfsPath::parse("/rlp/encode/r1/in.json").unwrap();
        let body = serde_json::json!({"value": ["0x83", "0xff", ["0x01"]]}).to_string();
        h.write(&in_path, body.as_bytes()).await.unwrap();
        let out_path = VfsPath::parse("/rlp/encode/r1/out.hex").unwrap();
        let v = h.read(&out_path).await.unwrap();
        let encoded = String::from_utf8(v).unwrap().trim().to_string();
        // Top-level list payload (6 bytes): item 0x83 -> 81 83, item 0xff
        // -> 81 ff, item [0x01] -> c1 01. Header c6 = 0xc0 + 6.
        assert_eq!(encoded, "0xc68183 81ff c101".replace(' ', ""));

        // Round-trip via decode session.
        let dec_in = VfsPath::parse("/rlp/decode/r1/in.json").unwrap();
        let body = serde_json::json!({"data": encoded}).to_string();
        h.write(&dec_in, body.as_bytes()).await.unwrap();
        let dec_out = VfsPath::parse("/rlp/decode/r1/out.json").unwrap();
        let v = h.read(&dec_out).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&v).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["hex"].as_str().unwrap(), "0x83");
    }

    #[tokio::test]
    async fn session_listing() {
        let h = ToolsHandler::new();
        let in_path = VfsPath::parse("/abi/encode/listed/in.json").unwrap();
        let body = serde_json::json!({"sig": "x()", "args": []}).to_string();
        h.write(&in_path, body.as_bytes()).await.unwrap();

        // Listing the family root shows the session id.
        let entries = h
            .list(&VfsPath::parse("/abi/encode").unwrap())
            .await
            .unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"listed"));

        // Listing the session dir shows in.json + out.hex.
        let entries = h
            .list(&VfsPath::parse("/abi/encode/listed").unwrap())
            .await
            .unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"in.json"));
        assert!(names.contains(&"out.hex"));
    }

    #[tokio::test]
    async fn missing_session_returns_not_found() {
        let h = ToolsHandler::new();
        let p = VfsPath::parse("/abi/encode/ghost/out.hex").unwrap();
        let err = h.read(&p).await.unwrap_err();
        match err {
            HandlerError::NotFound(_) => {}
            other => panic!("expected NotFound, got {:?}", other),
        }
    }
}
