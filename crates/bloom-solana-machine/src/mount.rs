//! Pinned, immutable Petal package mounting.
//!
//! The solana-driver is installed only when the package on disk matches the
//! committed content-addressed build manifest exactly: the manifest's own
//! package hash equals the pinned expectation, and every route artifact's
//! blake3 digest matches the file bytes. Any drift — rebuilt component,
//! edited manifest, swapped artifact — fails closed before installation.

use std::path::Path;
use std::sync::Arc;

use bloom_petals::{PetalRouter, PetalRunner, PetalStore, PetalVm};
use bloom_vfs::Vfs;
use serde::Deserialize;

/// The committed immutable package pin for the solana-driver. Any change to
/// the Petal source produces a different package hash and must update this
/// constant through review — there is no floating install.
pub const PINNED_SOLANA_DRIVER_PACKAGE_HASH: &str =
    "1c7b3173b8c04915abe5aca1db9a7f108b577903ad0cbd908f43154f78ae5e5f";

#[derive(Debug, Deserialize)]
struct BuildManifest {
    schema: String,
    source_package_hash: String,
    routes: Vec<BuildRoute>,
}

#[derive(Debug, Deserialize)]
struct BuildRoute {
    #[allow(dead_code)] // present in the schema; pinning uses the artifact digests
    route_id: String,
    #[allow(dead_code)]
    pattern: String,
    artifact_path: String,
    artifact_hash: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PinError {
    #[error("build manifest schema '{0}' is not bloom.petal.build-manifest.v1")]
    BadSchema(String),
    #[error(
        "package pin mismatch: manifest {manifest}, pinned {pinned} — refusing floating install"
    )]
    PackagePinMismatch { manifest: String, pinned: String },
    #[error("route artifact '{0}' does not match its recorded digest")]
    ArtifactDrift(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("petal store: {0}")]
    Store(#[from] bloom_petals::PetalError),
}

/// Trusted catalog verification keys: `(key id, verifying key)` pairs from
/// the operator's trusted-key file.
pub type TrustedCatalogKeys = Vec<(String, ed25519_dalek::VerifyingKey)>;

/// Load trusted keys from a JSON file: `[{"key_id": .., "verifying_key_base64": ..}]`.
pub fn load_trusted_catalog_keys(path: &Path) -> Result<TrustedCatalogKeys, PinError> {
    use base64::Engine as _;
    #[derive(serde::Deserialize)]
    struct KeyFile {
        #[serde(rename = "key_id")]
        key_id: String,
        #[serde(rename = "verifying_key_base64")]
        verifying_key_base64: String,
    }
    let raw = std::fs::read(path)?;
    let entries: Vec<KeyFile> = serde_json::from_slice(&raw)?;
    let mut keys = Vec::new();
    for entry in entries {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&entry.verifying_key_base64)
            .map_err(|e| PinError::Io(std::io::Error::other(e.to_string())))?;
        let arr: [u8; 32] = bytes.try_into().map_err(|v: Vec<u8>| {
            PinError::Io(std::io::Error::other(format!("key len {}", v.len())))
        })?;
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&arr)
            .map_err(|e| PinError::Io(std::io::Error::other(e.to_string())))?;
        keys.push((entry.key_id, vk));
    }
    Ok(keys)
}

/// Verify the package tree against its committed manifest and the pinned
/// package hash, then install and mount under `/petals`.
pub fn mount_pinned_solana_driver(
    petal_dir: &Path,
    state_root: &Path,
    host: Arc<dyn bloom_petals::PetalHost>,
) -> Result<Vfs, PinError> {
    let manifest_bytes = std::fs::read(petal_dir.join("artifacts/build-manifest.json"))?;
    let manifest: BuildManifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.schema != "bloom.petal.build-manifest.v1" {
        return Err(PinError::BadSchema(manifest.schema));
    }
    if manifest.source_package_hash != PINNED_SOLANA_DRIVER_PACKAGE_HASH {
        return Err(PinError::PackagePinMismatch {
            manifest: manifest.source_package_hash,
            pinned: PINNED_SOLANA_DRIVER_PACKAGE_HASH.to_string(),
        });
    }
    for route in &manifest.routes {
        let artifact = petal_dir.join(&route.artifact_path);
        let bytes = std::fs::read(&artifact)?;
        let digest = hex::encode(blake3::hash(&bytes).as_bytes());
        if digest != route.artifact_hash {
            return Err(PinError::ArtifactDrift(route.artifact_path.clone()));
        }
    }

    let store = PetalStore::open(state_root.join("petals"))?;
    store.install_petal_package_dir(petal_dir)?;
    let registry = Arc::new(bloom_petals::NameRegistry::open(
        state_root.join("registry"),
    )?);
    let runner = PetalRunner::new(store, registry, PetalVm::new()?);
    Ok(Vfs::builder()
        .mount("petals", Arc::new(PetalRouter::new(runner, host)))
        .build())
}

/// Content pin AND catalog-signature verified mount: the strongest posture.
/// Requires a signed catalog entry (artifacts/catalog-entry.json in the
/// package) that verifies under one of the trusted keys and passes every
/// install gate. `mount_pinned_solana_driver` alone means "content pinned";
/// this function additionally means "catalog signature verified".
pub fn mount_pinned_and_catalog_verified_solana_driver(
    petal_dir: &Path,
    state_root: &Path,
    host: Arc<dyn bloom_petals::PetalHost>,
    trusted_keys: &TrustedCatalogKeys,
    in_flight_predecessor_ops: &[String],
) -> Result<Vfs, PinError> {
    let entry_path = petal_dir.join("artifacts/catalog-entry.json");
    let raw = std::fs::read(&entry_path)?;
    let signed: crate::catalog::SignedCatalogEntry =
        serde_json::from_slice(&raw).map_err(PinError::Json)?;
    signed
        .verify(trusted_keys)
        .map_err(|e| PinError::Io(std::io::Error::other(format!("catalog: {e}"))))?;
    signed
        .entry
        .gate_install(petal_dir, in_flight_predecessor_ops)
        .map_err(|e| PinError::Io(std::io::Error::other(format!("catalog install gate: {e}"))))?;
    mount_pinned_solana_driver(petal_dir, state_root, host)
}
