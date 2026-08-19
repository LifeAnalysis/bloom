//! Read-only projection of the Solana transfer outbox.
//!
//! Mirrors the EVM outbox's read surface — `{pending,sent,failed}/<id>/` with
//! `intent.json` (the write-once staged transfer), `plan.md`, the reconciler's
//! `receipt.json`, and the broadcast-attempt marker — but over `bloom-solana-tx`'s
//! `SolanaOutbox` (Solana-typed intents, base58 keys, lamports).
//!
//! The handler is mounted at a dedicated top-level segment; its path shape is
//! `<wallet>/<chain>/<state>/<id>/<file>`, the Solana analogue of EVM's
//! `wallets/<wallet>/chains/<chain>/outbox/...`. The write path (`new.tx` stage,
//! `confirm` broadcast) belongs to the daemon's transfer engine and is not here.

use std::path::PathBuf;

use async_trait::async_trait;
use bloom_solana_tx::outbox::{SolanaOutbox, SolanaOutboxState};

use crate::handler::{Entry, Handler, HandlerError};

const ACTION_FILES: &[&str] = &[
    "intent.json",
    "plan.md",
    "receipt.json",
    "broadcast_attempted.json",
];

pub struct SolanaOutboxHandler {
    outbox: SolanaOutbox,
}

impl SolanaOutboxHandler {
    pub fn new(root: PathBuf) -> Result<Self, HandlerError> {
        Ok(Self {
            outbox: SolanaOutbox::new(root).map_err(|e| HandlerError::backend(e.to_string()))?,
        })
    }

    fn state(seg: &str) -> Option<SolanaOutboxState> {
        match seg {
            "pending" => Some(SolanaOutboxState::Pending),
            "sent" => Some(SolanaOutboxState::Sent),
            "failed" => Some(SolanaOutboxState::Failed),
            _ => None,
        }
    }

    fn read_staged(
        &self,
        wallet: &str,
        chain: &str,
        id: &str,
        state: SolanaOutboxState,
    ) -> Result<bloom_solana_tx::outbox::SolanaOutboxEntry, HandlerError> {
        self.outbox
            .read_in_state(wallet, chain, id, state)
            .map_err(|e| match e {
                bloom_solana_tx::outbox::OutboxError::NotFound(id) => HandlerError::not_found(id),
                other => HandlerError::backend(other.to_string()),
            })
    }
}

#[async_trait]
impl Handler for SolanaOutboxHandler {
    async fn lookup(&self, path: &crate::path::VfsPath) -> Result<Entry, HandlerError> {
        let segs: Vec<&str> = path.segments().iter().map(|s| s.as_str()).collect();
        match segs.as_slice() {
            [wallet, chain, state] if Self::state(state).is_some() => Ok(Entry::dir(state)),
            [wallet, chain, state, id] if Self::state(state).is_some() => {
                self.read_staged(wallet, chain, id, Self::state(state).unwrap())?;
                Ok(Entry::dir(id))
            }
            [wallet, chain, state, id, file] if Self::state(state).is_some() => {
                let entry = self.read_staged(wallet, chain, id, Self::state(state).unwrap())?;
                let fpath = entry.dir.join(file);
                if !ACTION_FILES.contains(file) || !fpath.exists() {
                    return Err(HandlerError::not_found(format!(
                        "/{wallet}/{chain}/{state}/{id}/{file}"
                    )));
                }
                Ok(Entry::file(file).with_fs_metadata(
                    &std::fs::metadata(&fpath).map_err(|e| HandlerError::backend(e.to_string()))?,
                ))
            }
            _ => Err(HandlerError::not_found(path.to_string_path())),
        }
    }

    async fn read(&self, path: &crate::path::VfsPath) -> Result<Vec<u8>, HandlerError> {
        let segs: Vec<&str> = path.segments().iter().map(|s| s.as_str()).collect();
        let [wallet, chain, state, id, file] = segs.as_slice() else {
            return Err(HandlerError::NotAFile(path.to_string_path()));
        };
        if !ACTION_FILES.contains(file) {
            return Err(HandlerError::not_found(path.to_string_path()));
        }
        let entry = self.read_staged(
            wallet,
            chain,
            id,
            Self::state(state).ok_or_else(|| HandlerError::not_found(path.to_string_path()))?,
        )?;
        let fpath = entry.dir.join(file);
        std::fs::read(&fpath).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HandlerError::not_found(fpath.to_string_lossy().to_string())
            } else {
                HandlerError::backend(e.to_string())
            }
        })
    }

    async fn write(&self, _path: &crate::path::VfsPath, _data: &[u8]) -> Result<(), HandlerError> {
        Err(HandlerError::PermissionDenied)
    }

    async fn list(&self, path: &crate::path::VfsPath) -> Result<Vec<Entry>, HandlerError> {
        let segs: Vec<&str> = path.segments().iter().map(|s| s.as_str()).collect();
        match segs.as_slice() {
            [_wallet, _chain] => Ok(vec![
                Entry::dir("pending"),
                Entry::dir("sent"),
                Entry::dir("failed"),
            ]),
            [wallet, chain, state] if Self::state(state).is_some() => {
                let ids = self
                    .outbox
                    .list(wallet, chain, Self::state(state).unwrap())
                    .map_err(|e| HandlerError::backend(e.to_string()))?;
                Ok(ids.into_iter().map(|id| Entry::dir(&id)).collect())
            }
            [wallet, chain, state, id] if Self::state(state).is_some() => {
                let entry = self.read_staged(wallet, chain, id, Self::state(state).unwrap())?;
                let mut entries: Vec<Entry> = Vec::new();
                for file in ACTION_FILES {
                    let fpath = entry.dir.join(file);
                    if fpath.exists() {
                        let metadata = std::fs::metadata(&fpath).ok();
                        entries.push(metadata.as_ref().map_or_else(
                            || Entry::file(file),
                            |m| Entry::file(file).with_fs_metadata(m),
                        ));
                    }
                }
                Ok(entries)
            }
            _ => Err(HandlerError::NotADir(path.to_string_path())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::VfsPath;

    fn staged(id: &str) -> bloom_solana_tx::types::StagedSolanaTransfer {
        bloom_solana_tx::types::StagedSolanaTransfer {
            id: id.to_string(),
            wallet: "alice".into(),
            chain: "solana-devnet".into(),
            fee_payer: "FEEPAYER111111111111111111111111111111111".into(),
            destination: "DEST111111111111111111111111111111111111111".into(),
            lamports: 1_000_000,
            blockhash: "BLOCKHASH111111111111111111111111111111111111".into(),
            last_valid_block_height: 100,
            message_b64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"m"),
            payload_digest_hex: "ab".repeat(32),
            signature: None,
            created_ms: 1,
            expires_ms: 0,
            status: bloom_solana_tx::types::SolanaTxStatus::Pending,
            action_id: None,
        }
    }

    #[tokio::test]
    async fn projects_staged_transfer_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = SolanaOutbox::new(dir.path().join("outbox")).unwrap();
        let s = staged("0001-00001");
        outbox.write_pending(&s, "plan").unwrap();

        let handler = SolanaOutboxHandler::new(dir.path().join("outbox")).unwrap();

        // List pending entries.
        let pending = handler
            .list(&VfsPath::parse("alice/solana-devnet/pending").unwrap())
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].name, "0001-00001");

        // Read the write-once intent.
        let intent = handler
            .read(&VfsPath::parse("alice/solana-devnet/pending/0001-00001/intent.json").unwrap())
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&intent).unwrap();
        assert_eq!(parsed["lamports"], 1_000_000);
        assert_eq!(parsed["destination"], s.destination);

        // The plan file is exposed.
        let plan = handler
            .read(&VfsPath::parse("alice/solana-devnet/pending/0001-00001/plan.md").unwrap())
            .await
            .unwrap();
        assert_eq!(plan, b"plan");

        // Writes are denied; unknown files/ids are not found.
        assert!(
            handler
                .write(
                    &VfsPath::parse("alice/solana-devnet/pending/0001-00001/intent.json").unwrap(),
                    b"x"
                )
                .await
                .is_err()
        );
        assert!(
            handler
                .lookup(
                    &VfsPath::parse("alice/solana-devnet/pending/0001-00001/secret.txt").unwrap()
                )
                .await
                .is_err()
        );
        assert!(
            handler
                .lookup(&VfsPath::parse("alice/solana-devnet/pending/9999-99999").unwrap())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn lists_states_and_entries() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = SolanaOutbox::new(dir.path().join("outbox")).unwrap();
        outbox.write_pending(&staged("0001-00001"), "plan").unwrap();
        let entry = outbox.read("alice", "solana-devnet", "0001-00001").unwrap();
        outbox.transition(&entry, SolanaOutboxState::Sent).unwrap();

        let handler = SolanaOutboxHandler::new(dir.path().join("outbox")).unwrap();
        let states = handler
            .list(&VfsPath::parse("alice/solana-devnet").unwrap())
            .await
            .unwrap();
        assert!(states.iter().any(|e| e.name == "sent"));
        let sent = handler
            .list(&VfsPath::parse("alice/solana-devnet/sent").unwrap())
            .await
            .unwrap();
        assert_eq!(sent.len(), 1);
    }
}
