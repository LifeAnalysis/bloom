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
    "975b4e8ac10f9b0f8da958dc02457db8bd08f895dabd844962d7130d1a7a13c7";

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
