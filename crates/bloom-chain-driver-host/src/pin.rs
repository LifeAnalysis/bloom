//! Content-addressed driver package pin verification.
//!
//! Generalized from the Solana driver's original pin code: any content pin
//! may be checked here, not one hardcoded package hash. The caller (Machine
//! boot code) supplies the pin — from installer-signed catalog metadata, an
//! operator config file, or a compiled-in constant for a single-driver
//! deployment — this module never originates one.

use std::path::Path;

use serde::Deserialize;

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

const BUILD_MANIFEST_SCHEMA: &str = "bloom.petal.build-manifest.v1";

#[derive(Debug, thiserror::Error)]
pub enum PinError {
    #[error("build manifest schema '{0}' is not {BUILD_MANIFEST_SCHEMA}")]
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
}

/// Verify a package tree on disk against its own committed build manifest
/// and a caller-supplied pinned package hash. Any drift — rebuilt
/// component, edited manifest, swapped artifact, wrong pin — fails closed.
/// Returns the verified route artifact paths on success.
pub fn verify_pinned_package(
    petal_dir: &Path,
    pinned_source_package_hash: &str,
) -> Result<Vec<String>, PinError> {
    let manifest_bytes = std::fs::read(petal_dir.join("artifacts/build-manifest.json"))?;
    let manifest: BuildManifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.schema != BUILD_MANIFEST_SCHEMA {
        return Err(PinError::BadSchema(manifest.schema));
    }
    if manifest.source_package_hash != pinned_source_package_hash {
        return Err(PinError::PackagePinMismatch {
            manifest: manifest.source_package_hash,
            pinned: pinned_source_package_hash.to_string(),
        });
    }
    let mut verified_paths = Vec::with_capacity(manifest.routes.len());
    for route in &manifest.routes {
        let artifact = petal_dir.join(&route.artifact_path);
        let bytes = std::fs::read(&artifact)?;
        let digest = hex::encode(blake3::hash(&bytes).as_bytes());
        if digest != route.artifact_hash {
            return Err(PinError::ArtifactDrift(route.artifact_path.clone()));
        }
        verified_paths.push(route.artifact_path.clone());
    }
    Ok(verified_paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, hash: &str, route_hash: &str) {
        std::fs::create_dir_all(dir.join("artifacts")).unwrap();
        std::fs::write(dir.join("route.wasm"), b"component-bytes").unwrap();
        let manifest = serde_json::json!({
            "schema": BUILD_MANIFEST_SCHEMA,
            "source_package_hash": hash,
            "routes": [{
                "route_id": "r1",
                "pattern": "/petals/x/*",
                "artifact_path": "route.wasm",
                "artifact_hash": route_hash,
            }],
        });
        std::fs::write(
            dir.join("artifacts/build-manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn real_hash() -> String {
        hex::encode(blake3::hash(b"component-bytes").as_bytes())
    }

    #[test]
    fn matching_pin_and_digests_verify() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "abc123", &real_hash());
        let routes = verify_pinned_package(dir.path(), "abc123").unwrap();
        assert_eq!(routes, vec!["route.wasm".to_string()]);
    }

    #[test]
    fn wrong_pin_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "abc123", &real_hash());
        assert!(matches!(
            verify_pinned_package(dir.path(), "different"),
            Err(PinError::PackagePinMismatch { .. })
        ));
    }

    #[test]
    fn drifted_artifact_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "abc123", "0000wronghash");
        assert!(matches!(
            verify_pinned_package(dir.path(), "abc123"),
            Err(PinError::ArtifactDrift(_))
        ));
    }

    #[test]
    fn bad_schema_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(
            dir.path().join("artifacts/build-manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": "wrong",
                "source_package_hash": "abc",
                "routes": [],
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            verify_pinned_package(dir.path(), "abc"),
            Err(PinError::BadSchema(_))
        ));
    }
}
