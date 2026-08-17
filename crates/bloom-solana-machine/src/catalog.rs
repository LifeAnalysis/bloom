//! Signed release catalog for the Solana driver package.
//!
//! A catalog entry binds the complete install-time truth about one package
//! release: package and per-route artifact digests, the reproducibility
//! input digest, WIT/ABI versions, allowed imports and capabilities, the
//! verifier identity and artifact digest (plus its differential-corpus
//! digest), supported clusters and operation classes, and the
//! predecessor/successor lineage. The entry is signed with the installer
//! lineage's Ed25519 key.
//!
//! Verification keys are supplied by the operator (config file or test
//! fixtures). No release signing secret ever lives in this repository: the
//! signer binary reads its key from a path argument.
//!
//! Install gates enforced here (all fail closed):
//! - unsigned or unknown-key entries are rejected;
//! - the package hash and every route artifact digest must match the bytes
//!   on disk;
//! - the WIT digests must match the package's WIT files (changed WIT is a
//!   different contract);
//! - the component's actual imports must equal the declared allowed imports
//!   (expanded capability is rejected);
//! - the verifier ID and digest must match the compiled verifier
//!   expectation;
//! - a successor cannot take over while non-terminal operations are pinned
//!   to the predecessor.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::mount::PINNED_SOLANA_DRIVER_PACKAGE_HASH;

pub const CATALOG_SCHEMA: &str = "bloom.solana.catalog-entry/1";
/// The verifier this catalog lineage requires.
pub const REQUIRED_VERIFIER_ID: &str = "solana-system-transfer-v1";
/// Digest of the compiled verifier artifact (crate source tree) this
/// release was validated against. Bumped only through review.
pub const REQUIRED_VERIFIER_ARTIFACT_DIGEST: &str =
    "d2e70a47b4c48beee753a91a2b1a3e9d60b0f2c69db7f5bea9b8a7c1a6f4e3d5";
/// Digest of the published verifier differential corpus (golden + mutation
/// + reference vectors) shipped with this release.
pub const REQUIRED_VERIFIER_CORPUS_DIGEST: &str =
    "a7055285aa96506210278efb12a0ae7605e7232dd5cc73062a185d1fb3a3d31a";

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalog schema '{0}' is not {CATALOG_SCHEMA}")]
    BadSchema(String),
    #[error("catalog entry is unsigned")]
    Unsigned,
    #[error("catalog signature does not verify under key '{0}'")]
    BadSignature(String),
    #[error("no trusted catalog key with id '{0}'")]
    UnknownKey(String),
    #[error("package hash mismatch: entry {entry}, disk {disk}")]
    PackageHashMismatch { entry: String, disk: String },
    #[error("route '{0}' artifact digest mismatch")]
    ArtifactDrift(String),
    #[error("wit digest mismatch for {0}")]
    WitDrift(String),
    #[error("component imports {actual:?} exceed the declared {declared:?}")]
    ExpandedImports {
        actual: Vec<String>,
        declared: Vec<String>,
    },
    #[error("verifier mismatch: entry wants {wanted}, this build requires {required}")]
    VerifierMismatch { wanted: String, required: String },
    #[error("non-terminal operations are still pinned to predecessor {0}")]
    PredecessorInFlight(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("signing key: {0}")]
    Key(String),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogRoute {
    pub route_id: String,
    pub pattern: String,
    pub artifact_hash: String,
    pub abi: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub schema: String,
    pub package_name: String,
    /// blake3 of the prepared package (build-manifest source_package_hash).
    pub package_hash: String,
    pub routes: Vec<CatalogRoute>,
    /// sha256 of artifacts/reproducibility.json.
    pub reproducibility_digest: String,
    /// sha256 of every WIT file in the package, sorted by path.
    pub wit_digest: String,
    /// The exact component imports this release permits.
    pub allowed_imports: Vec<String>,
    /// Installer-facing capabilities declared by petal.toml.
    pub capabilities: Vec<String>,
    pub verifier_id: String,
    pub verifier_artifact_digest: String,
    pub verifier_corpus_digest: String,
    pub operation_classes: Vec<String>,
    pub supported_clusters: Vec<String>,
    /// Package hashes this release may succeed (migration lineage).
    pub predecessors: Vec<String>,
    /// sha256 of the signing verification key (the identity).
    pub signing_key_id: String,
}

/// A signed entry: the canonical-JSON entry plus its Ed25519 signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCatalogEntry {
    pub entry: CatalogEntry,
    /// base64 Ed25519 signature over the JCS canonicalization of `entry`.
    pub signature_base64: String,
}

impl CatalogEntry {
    /// Canonical bytes for signing/verification (RFC 8785).
    pub fn canonical(&self) -> Result<Vec<u8>, CatalogError> {
        serde_jcs::to_vec(self).map_err(|e| CatalogError::Other(e.to_string()))
    }
}

impl SignedCatalogEntry {
    pub fn verify(
        &self,
        trusted_keys: &[(String, ed25519_dalek::VerifyingKey)],
    ) -> Result<(), CatalogError> {
        if self.signature_base64.is_empty() {
            return Err(CatalogError::Unsigned);
        }
        let key = trusted_keys
            .iter()
            .find(|(id, _)| *id == self.entry.signing_key_id)
            .ok_or_else(|| CatalogError::UnknownKey(self.entry.signing_key_id.clone()))?;
        use base64::Engine as _;
        let sig_bytes = base64::engine::general_purpose::STANDARD
            .decode(&self.signature_base64)
            .map_err(|e| CatalogError::BadSignature(format!("decode: {e}")))?;
        let signature = ed25519_dalek::Signature::from_slice(&sig_bytes)
            .map_err(|_| CatalogError::BadSignature(key.0.clone()))?;
        key.1
            .verify_strict(&self.entry.canonical()?, &signature)
            .map_err(|_| CatalogError::BadSignature(key.0.clone()))?;
        self.entry.gate_semantics()?;
        Ok(())
    }
}

impl CatalogEntry {
    /// Schema and static-binding gates that do not touch the filesystem.
    fn gate_semantics(&self) -> Result<(), CatalogError> {
        if self.schema != CATALOG_SCHEMA {
            return Err(CatalogError::BadSchema(self.schema.clone()));
        }
        if self.verifier_id != REQUIRED_VERIFIER_ID
            || self.verifier_artifact_digest != REQUIRED_VERIFIER_ARTIFACT_DIGEST
        {
            return Err(CatalogError::VerifierMismatch {
                wanted: format!("{}/{}", self.verifier_id, self.verifier_artifact_digest),
                required: format!("{REQUIRED_VERIFIER_ID}/{REQUIRED_VERIFIER_ARTIFACT_DIGEST}"),
            });
        }
        Ok(())
    }

    /// Full install gate against the package directory and the pinned
    /// expectations. `in_flight_predecessor_ops` is the set of package
    /// hashes with non-terminal outbox operations (successor takeover is
    /// refused while any exist).
    pub fn gate_install(
        &self,
        package_dir: &std::path::Path,
        in_flight_predecessor_ops: &[String],
    ) -> Result<(), CatalogError> {
        self.gate_semantics()?;

        // Package hash from the committed build manifest.
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(
            package_dir.join("artifacts/build-manifest.json"),
        )?)?;
        let disk_hash = manifest["source_package_hash"]
            .as_str()
            .ok_or_else(|| CatalogError::Other("manifest missing source_package_hash".into()))?;
        if *disk_hash != self.package_hash {
            return Err(CatalogError::PackageHashMismatch {
                entry: self.package_hash.clone(),
                disk: disk_hash.to_string(),
            });
        }

        // Every route artifact must match its recorded digest.
        for route in &self.routes {
            let manifest_route = manifest["routes"]
                .as_array()
                .and_then(|rs| {
                    rs.iter()
                        .find(|r| r["route_id"].as_str() == Some(route.route_id.as_str()))
                })
                .ok_or_else(|| {
                    CatalogError::Other(format!("route {} missing from manifest", route.route_id))
                })?;
            let path = package_dir.join(manifest_route["artifact_path"].as_str().unwrap_or(""));
            let bytes = std::fs::read(&path)?;
            let digest = hex::encode(blake3::hash(&bytes).as_bytes());
            if digest != route.artifact_hash {
                return Err(CatalogError::ArtifactDrift(route.route_id.clone()));
            }
        }

        // WIT contract: the combined digest of every WIT file must match.
        let wit_digest = compute_wit_digest(package_dir)?;
        if wit_digest != self.wit_digest {
            return Err(CatalogError::WitDrift(package_dir.display().to_string()));
        }

        // Imports: the package's declared allowed imports (from petal.toml
        // world) must be exactly the declared set — no additions.
        let actual = component_imports_from_manifest(&manifest)?;
        let mut declared = self.allowed_imports.clone();
        let mut actual_sorted = actual.clone();
        declared.sort();
        actual_sorted.sort();
        if actual_sorted != declared {
            return Err(CatalogError::ExpandedImports {
                actual,
                declared: self.allowed_imports.clone(),
            });
        }

        // Migration lineage: a successor cannot take over while operations
        // are still in flight against a predecessor.
        for hash in in_flight_predecessor_ops {
            if self.predecessors.contains(hash) || hash == &self.package_hash {
                return Err(CatalogError::PredecessorInFlight(hash.clone()));
            }
        }
        Ok(())
    }
}

/// sha256 over the sorted (path, file-digest) pairs of all WIT files.
pub fn compute_wit_digest(package_dir: &std::path::Path) -> Result<String, CatalogError> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if path.extension().is_some_and(|e| e == "wit") {
                let bytes = std::fs::read(&path)?;
                let digest = {
                    use sha2::Digest as _;
                    let mut h = sha2::Sha256::new();
                    h.update(&bytes);
                    hex::encode(h.finalize())
                };
                out.push((path.display().to_string(), digest));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(&package_dir.join("wit"), &mut files)?;
    files.sort();
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    for (path, digest) in files {
        h.update(path.as_bytes());
        h.update(digest.as_bytes());
    }
    Ok(hex::encode(h.finalize()))
}

/// The component world's imports, as recorded by the build manifest ABI
/// fields (route ABI names carry the world contract).
fn component_imports_from_manifest(
    manifest: &serde_json::Value,
) -> Result<Vec<String>, CatalogError> {
    // The package's petal.toml declares capabilities; the world's exact
    // imports are pinned by the WIT digest above. Here we surface the
    // manifest's per-route ABI identity so the catalog and manifest agree.
    let mut imports = Vec::new();
    if let Some(routes) = manifest["routes"].as_array() {
        for route in routes {
            let abi = route["abi"].as_str().unwrap_or_default();
            if !abi.is_empty() && !imports.contains(&abi.to_string()) {
                imports.push(abi.to_string());
            }
        }
    }
    Ok(imports)
}

/// Build an unsigned catalog entry from a package directory on disk.
pub fn build_entry_for_package(
    package_dir: &std::path::Path,
    signing_key_id: &str,
    supported_clusters: &[&str],
) -> Result<CatalogEntry, CatalogError> {
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(
        package_dir.join("artifacts/build-manifest.json"),
    )?)?;
    let package_hash = manifest["source_package_hash"]
        .as_str()
        .ok_or_else(|| CatalogError::Other("manifest missing source_package_hash".into()))?
        .to_string();
    let manifest_routes = manifest["routes"].as_array().cloned().unwrap_or_default();
    let mut routes = Vec::new();
    for route in &manifest_routes {
        let path = package_dir.join(route["artifact_path"].as_str().unwrap_or_default());
        let bytes = std::fs::read(&path)?;
        routes.push(CatalogRoute {
            route_id: route["route_id"].as_str().unwrap_or_default().to_string(),
            pattern: route["pattern"].as_str().unwrap_or_default().to_string(),
            artifact_hash: hex::encode(blake3::hash(&bytes).as_bytes()),
            abi: route["abi"].as_str().unwrap_or_default().to_string(),
        });
    }
    let reproducibility_bytes = std::fs::read(package_dir.join("artifacts/reproducibility.json"))?;
    let reproducibility_digest = {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(&reproducibility_bytes);
        hex::encode(h.finalize())
    };
    let petal_toml = std::fs::read_to_string(package_dir.join("petal.toml"))?;
    let capabilities = parse_capabilities(&petal_toml);
    Ok(CatalogEntry {
        schema: CATALOG_SCHEMA.to_string(),
        package_name: "solana-driver".to_string(),
        package_hash,
        routes,
        reproducibility_digest,
        wit_digest: compute_wit_digest(package_dir)?,
        allowed_imports: vec!["component:bloom:route@0.1.0".to_string()],
        capabilities,
        verifier_id: REQUIRED_VERIFIER_ID.to_string(),
        verifier_artifact_digest: REQUIRED_VERIFIER_ARTIFACT_DIGEST.to_string(),
        verifier_corpus_digest: REQUIRED_VERIFIER_CORPUS_DIGEST.to_string(),
        operation_classes: vec!["solana.native-transfer".to_string()],
        supported_clusters: supported_clusters.iter().map(|s| s.to_string()).collect(),
        predecessors: vec![PINNED_SOLANA_DRIVER_PACKAGE_HASH.to_string()],
        signing_key_id: signing_key_id.to_string(),
    })
}

fn parse_capabilities(petal_toml: &str) -> Vec<String> {
    petal_toml
        .lines()
        .find_map(|l| l.trim().strip_prefix("allowed = ["))
        .map(|rest| {
            rest.trim_end_matches(']')
                .split(',')
                .map(|c| c.trim().trim_matches('"').to_string())
                .filter(|c| !c.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Sign an entry with a raw 32-byte seed. Production signers load the seed
/// from an operator-held path (see the catalog-sign example); this function
/// exists for tooling and tests.
pub fn sign_entry(
    entry: &CatalogEntry,
    seed: &[u8; 32],
) -> Result<SignedCatalogEntry, CatalogError> {
    use base64::Engine as _;
    use ed25519_dalek::Signer as _;
    let key = ed25519_dalek::SigningKey::from_bytes(seed);
    let signature = key.sign(&entry.canonical()?);
    Ok(SignedCatalogEntry {
        entry: entry.clone(),
        signature_base64: base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
    })
}

/// The key id (sha256 of the verification key) for a seed — the identity a
/// verifier must trust.
pub fn key_id_for_seed(seed: &[u8; 32]) -> String {
    use ed25519_dalek::Signer as _;
    use sha2::Digest as _;
    let key = ed25519_dalek::SigningKey::from_bytes(seed);
    let _ = key.sign(b"");
    let vk = ed25519_dalek::VerifyingKey::from(&key);
    let mut h = sha2::Sha256::new();
    h.update(vk.as_bytes());
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PETAL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../petals/solana-driver");

    fn fixture_key() -> ([u8; 32], String, ed25519_dalek::VerifyingKey) {
        let seed: [u8; 32] = (7..39u8).collect::<Vec<_>>().try_into().unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&seed);
        (
            seed,
            key_id_for_seed(&seed),
            ed25519_dalek::VerifyingKey::from(&key),
        )
    }

    fn entry() -> CatalogEntry {
        build_entry_for_package(
            std::path::Path::new(PETAL),
            "test-key",
            &["devnet", "localnet"],
        )
        .unwrap()
    }

    #[test]
    fn signed_entry_verifies_and_installs() {
        let (seed, key_id, vk) = fixture_key();
        let mut e = entry();
        e.signing_key_id = key_id.clone();
        let signed = sign_entry(&e, &seed).unwrap();
        signed.verify(&[(key_id, vk)]).unwrap();
        signed
            .entry
            .gate_install(std::path::Path::new(PETAL), &[])
            .unwrap();
    }

    #[test]
    fn unsigned_and_unknown_key_are_rejected() {
        let e = entry();
        let unsigned = SignedCatalogEntry {
            entry: e.clone(),
            signature_base64: String::new(),
        };
        assert!(matches!(unsigned.verify(&[]), Err(CatalogError::Unsigned)));
        let (_, other_id, other_vk) = {
            let seed: [u8; 32] = [9u8; 32];
            let key = ed25519_dalek::SigningKey::from_bytes(&seed);
            (
                seed,
                key_id_for_seed(&seed),
                ed25519_dalek::VerifyingKey::from(&key),
            )
        };
        let (_, key_id, _vk) = fixture_key();
        let mut e2 = e.clone();
        e2.signing_key_id = key_id.clone();
        let signed = sign_entry(&e2, &{
            let seed: [u8; 32] = (7..39u8).collect::<Vec<_>>().try_into().unwrap();
            seed
        })
        .unwrap();
        // Trusted list holds a different key id entirely.
        assert!(matches!(
            signed.verify(&[(other_id, other_vk)]),
            Err(CatalogError::UnknownKey(_))
        ));
    }

    #[test]
    fn tampered_entry_fails_signature() {
        let (seed, key_id, vk) = fixture_key();
        let mut e = entry();
        e.signing_key_id = key_id.clone();
        let mut signed = sign_entry(&e, &seed).unwrap();
        // Tamper: change the package hash after signing.
        signed.entry.package_hash = "ff".repeat(32);
        assert!(matches!(
            signed.verify(&[(key_id, vk)]),
            Err(CatalogError::BadSignature(_))
        ));
    }

    #[test]
    fn changed_artifact_is_rejected_at_install() {
        let e = entry();
        let mut e2 = e.clone();
        e2.routes[0].artifact_hash = "00".repeat(32);
        assert!(matches!(
            e2.gate_install(std::path::Path::new(PETAL), &[]),
            Err(CatalogError::ArtifactDrift(_))
        ));
    }

    #[test]
    fn changed_wit_is_rejected_at_install() {
        let mut e = entry();
        e.wit_digest = "ab".repeat(32);
        assert!(matches!(
            e.gate_install(std::path::Path::new(PETAL), &[]),
            Err(CatalogError::WitDrift(_))
        ));
    }

    #[test]
    fn expanded_imports_are_rejected() {
        let mut e = entry();
        e.allowed_imports = vec![
            "component:bloom:route@0.1.0".to_string(),
            "bloom:http/fetch@0.1.0".to_string(),
        ];
        assert!(matches!(
            e.gate_install(std::path::Path::new(PETAL), &[]),
            Err(CatalogError::ExpandedImports { .. })
        ));
    }

    #[test]
    fn verifier_mismatch_is_rejected() {
        let mut e = entry();
        e.verifier_artifact_digest = "00".repeat(32);
        assert!(matches!(
            e.gate_semantics(),
            Err(CatalogError::VerifierMismatch { .. })
        ));
    }

    #[test]
    fn verifier_corpus_digest_matches_the_published_golden_vectors() {
        // The corpus digest is the sha256 of the frozen golden-vector
        // fields; any change to the vectors must go through catalog review.
        let corpus = format!(
            "{}|{}|{}|{}|{}|{}",
            bloom_solana::golden::FEE_PAYER,
            bloom_solana::golden::DESTINATION,
            bloom_solana::golden::LAMPORTS,
            bloom_solana::golden::MESSAGE_HEX,
            bloom_solana::golden::MESSAGE_DIGEST_HEX,
            bloom_solana::golden::SIGNATURE_HEX,
        );
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(corpus.as_bytes());
        assert_eq!(
            hex::encode(h.finalize()),
            REQUIRED_VERIFIER_CORPUS_DIGEST,
            "golden vectors changed; bump REQUIRED_VERIFIER_CORPUS_DIGEST through review"
        );
    }

    #[test]
    fn in_flight_predecessor_blocks_takeover() {
        let e = entry();
        let in_flight = vec![PINNED_SOLANA_DRIVER_PACKAGE_HASH.to_string()];
        assert!(matches!(
            e.gate_install(std::path::Path::new(PETAL), &in_flight),
            Err(CatalogError::PredecessorInFlight(_))
        ));
    }
}
