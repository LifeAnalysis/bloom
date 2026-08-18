//! Owner-only projection of the durable chain-action outbox.
//!
//! Read-only view over the chain-neutral outbox (`bloom-chain-action`):
//! `/chain-actions/` lists staged operations, and each action exposes its
//! immutable envelope plus a derived `status.json` (state, provenance
//! digests, broadcast attempts). Guests never see this tree — the mount is
//! owner-only, enforced by the daemon's guest-path authorization.

use std::path::PathBuf;

use async_trait::async_trait;
use bloom_chain_action::{ChainActionOutbox, OutboxError};

use crate::handler::{Entry, Handler, HandlerError};
use crate::path::VfsPath;

pub struct ChainActionsHandler {
    outbox: ChainActionOutbox,
}

impl ChainActionsHandler {
    pub fn new(root: PathBuf) -> Result<Self, OutboxError> {
        Ok(Self {
            outbox: ChainActionOutbox::new(root)?,
        })
    }

    fn is_operation_id(id: &str) -> bool {
        id.len() == 64
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    fn backend(error: OutboxError) -> HandlerError {
        match error {
            OutboxError::NotFound(what) => HandlerError::NotFound(what),
            other => HandlerError::backend(format!("chain-action outbox: {other}")),
        }
    }

    fn status_json(action: &bloom_chain_action::Action) -> serde_json::Value {
        serde_json::json!({
            "schema": "bloom.machine.chain-action-status.v1",
            "operation_id": action.envelope.operation_id,
            "state": action.state.as_str(),
            "terminal": action.state.is_terminal(),
            "driver": {
                "package_hash": action.envelope.driver.package_hash,
                "route": action.envelope.driver.route,
            },
            "wallet_id": action.envelope.wallet_id,
            "chain": {
                "family": action.envelope.chain.family,
                "profile": action.envelope.chain.profile,
                "claimed_caip2": action.envelope.chain.claimed_caip2,
            },
            "operation_class": action.envelope.operation_class,
            "crypto_suite": action.envelope.crypto_suite,
            "payload_digest": action.envelope.payload_digest_hex,
            "artifact_digest": action.artifact.as_ref().map(|a| a.digest_hex.clone()),
            "broadcast_attempts": action.attempts.iter().map(|a| serde_json::json!({
                "attempt": a.attempt,
                "artifact_digest": a.artifact_digest_hex,
                "outcome": match &a.outcome {
                    None => serde_json::Value::Null,
                    Some(bloom_chain_action::AttemptOutcome::Accepted) =>
                        serde_json::json!("accepted"),
                    Some(bloom_chain_action::AttemptOutcome::Ambiguous) =>
                        serde_json::json!("ambiguous"),
                    Some(bloom_chain_action::AttemptOutcome::Rejected { reason }) =>
                        serde_json::json!({ "rejected": reason }),
                },
            })).collect::<Vec<_>>(),
        })
    }
}

#[async_trait]
impl Handler for ChainActionsHandler {
    async fn lookup(&self, path: &VfsPath) -> Result<Entry, HandlerError> {
        let segs: Vec<&str> = path.segments().iter().map(|s| s.as_str()).collect();
        match segs.as_slice() {
            [] => Ok(Entry::dir("chain-actions")),
            [id] if Self::is_operation_id(id) => {
                self.outbox.load(id).map_err(Self::backend)?;
                Ok(Entry::dir(id))
            }
            [id, file] if Self::is_operation_id(id) => match *file {
                "envelope.json" | "status.json" => {
                    self.outbox.load(id).map_err(Self::backend)?;
                    Ok(Entry::file(file))
                }
                _ => Err(HandlerError::NotFound(format!(
                    "/chain-actions/{id}/{file}"
                ))),
            },
            _ => Err(HandlerError::NotFound(path.to_string_path())),
        }
    }

    async fn read(&self, path: &VfsPath) -> Result<Vec<u8>, HandlerError> {
        let segs: Vec<&str> = path.segments().iter().map(|s| s.as_str()).collect();
        let [id, file] = segs.as_slice() else {
            return Err(HandlerError::NotAFile(path.to_string_path()));
        };
        if !Self::is_operation_id(id) {
            return Err(HandlerError::NotFound(path.to_string_path()));
        }
        let action = self.outbox.load(id).map_err(Self::backend)?;
        match *file {
            "envelope.json" => serde_json::to_vec_pretty(&action.envelope)
                .map_err(|e| HandlerError::backend(format!("encode envelope: {e}"))),
            "status.json" => serde_json::to_vec_pretty(&Self::status_json(&action))
                .map_err(|e| HandlerError::backend(format!("encode status: {e}"))),
            _ => Err(HandlerError::NotFound(format!(
                "/chain-actions/{id}/{file}"
            ))),
        }
    }

    async fn write(&self, _path: &VfsPath, _data: &[u8]) -> Result<(), HandlerError> {
        Err(HandlerError::PermissionDenied)
    }

    async fn list(&self, path: &VfsPath) -> Result<Vec<Entry>, HandlerError> {
        let segs: Vec<&str> = path.segments().iter().map(|s| s.as_str()).collect();
        match segs.as_slice() {
            [] => {
                let mut entries: Vec<Entry> = self
                    .outbox
                    .list()
                    .map_err(Self::backend)?
                    .into_iter()
                    .map(|id| Entry::dir(&id))
                    .collect();
                entries.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(entries)
            }
            [id] if Self::is_operation_id(id) => {
                self.outbox.load(id).map_err(Self::backend)?;
                Ok(vec![
                    Entry::file("envelope.json"),
                    Entry::file("status.json"),
                ])
            }
            _ => Err(HandlerError::NotADir(path.to_string_path())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(outbox: &ChainActionOutbox, id: &str) {
        use bloom_chain_action::{
            ArtifactSegment, ArtifactTemplate, ChainBinding, DriverBinding, NewAction,
        };
        let payload = b"message-bytes".to_vec();
        outbox
            .stage(NewAction {
                operation_id: id.to_string(),
                idempotency_key: format!("idem-{id}"),
                driver: DriverBinding {
                    package_hash: "c".repeat(64),
                    route: "transfer.stage.json".to_string(),
                    abi_version: 1,
                    state_schema: 1,
                },
                wallet_id: "wallet-1".to_string(),
                key_ref: "broker-exact-selection".to_string(),
                chain: ChainBinding {
                    family: "solana".to_string(),
                    profile: "solana-devnet".to_string(),
                    claimed_caip2: "solana:devnet".to_string(),
                },
                operation_class: "solana.native-transfer".to_string(),
                crypto_suite: "ed25519-message".to_string(),
                artifact_template: ArtifactTemplate {
                    segments: vec![
                        ArtifactSegment::Literal {
                            bytes_hex: "01".to_string(),
                        },
                        ArtifactSegment::Signature { index: 0 },
                        ArtifactSegment::Literal {
                            bytes_hex: hex::encode(&payload),
                        },
                    ],
                },
                payload,
                created_at_ms: 1,
                expires_at_ms: 0,
            })
            .unwrap();
    }

    #[tokio::test]
    async fn projects_envelope_and_status_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let outbox = ChainActionOutbox::new(tmp.path().join("chain-actions")).unwrap();
        let id = format!("{:064x}", 1u8);
        stage(&outbox, &id);
        let handler = ChainActionsHandler::new(tmp.path().join("chain-actions")).unwrap();

        let listed = handler.list(&VfsPath::root()).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, id);

        let files = handler.list(&VfsPath::parse(&id).unwrap()).await.unwrap();
        assert_eq!(files.len(), 2);

        let status: serde_json::Value = serde_json::from_slice(
            &handler
                .read(&VfsPath::parse(&format!("{id}/status.json")).unwrap())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(status["state"], "staged");
        assert_eq!(status["chain"]["profile"], "solana-devnet");
        assert_eq!(
            status["payload_digest"],
            outbox.load(&id).unwrap().envelope.payload_digest_hex
        );

        let envelope: serde_json::Value = serde_json::from_slice(
            &handler
                .read(&VfsPath::parse(&format!("{id}/envelope.json")).unwrap())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(envelope["schema"], "bloom.chain-action/2");

        // Writes are denied; unknown files and ids are NotFound.
        assert!(
            handler
                .write(&VfsPath::parse(&format!("{id}/status.json")).unwrap(), b"x")
                .await
                .is_err()
        );
        assert!(
            handler
                .lookup(&VfsPath::parse(&format!("{id}/journal.json")).unwrap())
                .await
                .is_err()
        );
        assert!(
            handler
                .lookup(&VfsPath::parse(&format!("{:064x}", 9u8)).unwrap())
                .await
                .is_err()
        );
    }
}
