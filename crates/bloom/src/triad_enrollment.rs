//! Root-only generation of per-login macOS triad identities and signing
//! material from public release templates.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write as _,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use bloom_triad_protocol::{
    Base64UrlBytes, PROVENANCE_RECORD_SIGNATURE_DOMAIN, ProvenanceCatalog, Token,
};
use ed25519_dalek::{Signer as _, SigningKey};
use rand::{RngCore as _, rngs::OsRng};
use serde::Serialize;
use zeroize::Zeroize as _;

const MAX_TEMPLATE_BYTES: u64 = 1024 * 1024;
const PUBLIC_TEMPLATE_FILES: [&str; 3] =
    ["edge-manifest.json.in", "broker.json.in", "signer.json.in"];

pub fn run_from_process_args() -> Result<()> {
    if rustix::process::geteuid().as_raw() != 0 {
        bail!("macOS enrollment material generation requires root");
    }
    if std::env::consts::OS != "macos" {
        bail!("macOS enrollment material generation requires Darwin");
    }
    let args = std::env::args_os().collect::<Vec<_>>();
    if args.len() != 9 {
        bail!("invalid macOS enrollment material invocation");
    }
    generate(&EnrollmentPlan {
        template_dir: PathBuf::from(&args[2]),
        output_dir: PathBuf::from(&args[3]),
        login_uid: decimal_arg(&args[4], "login UID")?,
        broker_uid: decimal_arg(&args[5], "Broker UID")?,
        signer_uid: decimal_arg(&args[6], "Signer UID")?,
        session_socket_gid: decimal_arg(&args[7], "session socket GID")?,
        release_digest: args[8]
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("release digest is not UTF-8"))?
            .to_owned(),
    })
}

#[derive(Clone, Debug)]
struct EnrollmentPlan {
    template_dir: PathBuf,
    output_dir: PathBuf,
    login_uid: u32,
    broker_uid: u32,
    signer_uid: u32,
    session_socket_gid: u32,
    release_digest: String,
}

fn generate(plan: &EnrollmentPlan) -> Result<()> {
    generate_for_owner(plan, 0)
}

fn generate_for_owner(plan: &EnrollmentPlan, expected_owner: u32) -> Result<()> {
    validate_plan(plan, expected_owner)?;
    let machine = ApplicationIdentity::generate("bloom-machine", plan.login_uid);
    let broker = ApplicationIdentity::generate("bloom-broker", plan.login_uid);
    let signer = ApplicationIdentity::generate("bloom-signer", plan.login_uid);
    let revoke = ApplicationIdentity::generate("bloom-revoke-client", plan.login_uid);
    let session = ApplicationIdentity::generate("bloom-session", plan.login_uid);
    let broker_signing = GeneratedKey::new();
    let broker_audit = GeneratedKey::new();
    let broker_review = GeneratedKey::new();
    let signer_revocation = GeneratedKey::new();
    let signer_ceremony = GeneratedKey::new();
    let installer = GeneratedKey::new();

    let replacements = SecretReplacements(BTreeMap::from([
        ("@LOGIN_UID@", plan.login_uid.to_string()),
        ("@BLOOM_BROKER_UID@", plan.broker_uid.to_string()),
        ("@BLOOM_SIGNER_UID@", plan.signer_uid.to_string()),
        ("@SESSION_SOCKET_GID@", plan.session_socket_gid.to_string()),
        ("@BUILD_DIGEST@", plan.release_digest.clone()),
        ("@MACHINE_BOOT_EPOCH@", machine.boot_epoch.clone()),
        (
            "@MACHINE_APPLICATION_KEY_ID@",
            machine.application_key_id.clone(),
        ),
        (
            "@MACHINE_APPLICATION_PUBLIC_KEY_HEX@",
            machine.key.public_hex(),
        ),
        ("@BROKER_BOOT_EPOCH@", broker.boot_epoch.clone()),
        (
            "@BROKER_APPLICATION_KEY_ID@",
            broker.application_key_id.clone(),
        ),
        (
            "@BROKER_APPLICATION_PUBLIC_KEY_HEX@",
            broker.key.public_hex(),
        ),
        ("@SIGNER_BOOT_EPOCH@", signer.boot_epoch.clone()),
        (
            "@SIGNER_APPLICATION_KEY_ID@",
            signer.application_key_id.clone(),
        ),
        (
            "@SIGNER_APPLICATION_PUBLIC_KEY_HEX@",
            signer.key.public_hex(),
        ),
        ("@REVOKE_BOOT_EPOCH@", revoke.boot_epoch.clone()),
        (
            "@REVOKE_APPLICATION_KEY_ID@",
            revoke.application_key_id.clone(),
        ),
        (
            "@REVOKE_APPLICATION_PUBLIC_KEY_HEX@",
            revoke.key.public_hex(),
        ),
        ("@SESSION_BOOT_EPOCH@", session.boot_epoch.clone()),
        (
            "@SESSION_APPLICATION_KEY_ID@",
            session.application_key_id.clone(),
        ),
        (
            "@SESSION_APPLICATION_PUBLIC_KEY_HEX@",
            session.key.public_hex(),
        ),
        ("@BROKER_SIGNING_SEED_HEX@", broker_signing.private_hex()),
        (
            "@BROKER_SIGNING_PUBLIC_KEY_HEX@",
            broker_signing.public_hex(),
        ),
        ("@BROKER_AUDIT_SEED_HEX@", broker_audit.private_hex()),
        ("@BROKER_REVIEW_SEED_HEX@", broker_review.private_hex()),
        ("@BROKER_REVIEW_PUBLIC_KEY_HEX@", broker_review.public_hex()),
        (
            "@SIGNER_REVOCATION_SEED_HEX@",
            signer_revocation.private_hex(),
        ),
        (
            "@SIGNER_REVOCATION_PUBLIC_KEY_HEX@",
            signer_revocation.public_hex(),
        ),
        ("@SIGNER_CEREMONY_SEED_HEX@", signer_ceremony.private_hex()),
        (
            "@SIGNER_CEREMONY_PUBLIC_KEY_HEX@",
            signer_ceremony.public_hex(),
        ),
        ("@INSTALLER_PUBLIC_KEY_HEX@", installer.public_hex()),
    ]));

    for name in PUBLIC_TEMPLATE_FILES {
        let mut rendered = render_public_template(&plan.template_dir.join(name), &replacements.0)?;
        let result = write_new_private(
            &plan.output_dir.join(name.trim_end_matches(".in")),
            rendered.as_bytes(),
        );
        rendered.zeroize();
        result?;
    }
    write_identity(&plan.output_dir.join("machine-identity.json"), &machine)?;
    write_identity(&plan.output_dir.join("broker-identity.json"), &broker)?;
    write_identity(&plan.output_dir.join("signer-identity.json"), &signer)?;
    write_identity(&plan.output_dir.join("revoke-identity.json"), &revoke)?;
    write_identity(&plan.output_dir.join("session-identity.json"), &session)?;

    let installer_key_id = format!("bloom-installer-{}", plan.login_uid);
    write_installer_identity(
        &plan.output_dir.join("installer-identity.json"),
        &installer_key_id,
        &installer,
    )?;
    sign_provenance_catalog(
        &plan.template_dir.join("provenance-catalog.unsigned.json"),
        &plan.output_dir.join("provenance-catalog.json"),
        &installer_key_id,
        &installer,
    )?;

    Ok(())
}

fn validate_plan(plan: &EnrollmentPlan, expected_owner: u32) -> Result<()> {
    if plan.login_uid == 0
        || plan.broker_uid == 0
        || plan.signer_uid == 0
        || plan.session_socket_gid == 0
        || plan.release_digest.len() != 64
        || !plan
            .release_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("macOS enrollment generation plan has invalid IDs or release digest");
    }
    let metadata = fs::symlink_metadata(&plan.output_dir).with_context(|| {
        format!(
            "inspect enrollment output directory {}",
            plan.output_dir.display()
        )
    })?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != expected_owner
        || metadata.mode() & 0o7777 != 0o700
        || fs::read_dir(&plan.output_dir)?.next().is_some()
    {
        bail!("enrollment output must be an empty root-owned mode-0700 directory");
    }
    for name in PUBLIC_TEMPLATE_FILES
        .into_iter()
        .chain(["provenance-catalog.unsigned.json"])
    {
        require_public_template(&plan.template_dir.join(name), expected_owner)?;
    }
    Ok(())
}

fn require_public_template(path: &Path, expected_owner: u32) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect public template {}", path.display()))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != expected_owner
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
        || metadata.len() > MAX_TEMPLATE_BYTES
    {
        bail!("enrollment template is not an immutable root-owned regular file");
    }
    Ok(())
}

fn render_public_template(path: &Path, replacements: &BTreeMap<&str, String>) -> Result<String> {
    let mut rendered =
        String::from_utf8(read_public_template(path)?).context("public template is not UTF-8")?;
    for (placeholder, value) in replacements {
        rendered = rendered.replace(placeholder, value);
    }
    for forbidden in [
        "_SEED_HEX@",
        "_BOOT_EPOCH@",
        "_APPLICATION_KEY_ID@",
        "_APPLICATION_PUBLIC_KEY_HEX@",
        "@INSTALLER_PUBLIC_KEY_HEX@",
        "@BUILD_DIGEST@",
    ] {
        if rendered.contains(forbidden) {
            rendered.zeroize();
            bail!("public template contains an unresolved security placeholder");
        }
    }
    if let Err(error) = serde_json::from_str::<serde_json::Value>(&rendered) {
        rendered.zeroize();
        return Err(error).context("rendered enrollment template is not JSON");
    }
    Ok(rendered)
}

fn read_public_template(path: &Path) -> Result<Vec<u8>> {
    let bytes =
        fs::read(path).with_context(|| format!("read public template {}", path.display()))?;
    if bytes.len() as u64 > MAX_TEMPLATE_BYTES {
        bail!("public enrollment template exceeds 1 MiB");
    }
    Ok(bytes)
}

fn write_identity(path: &Path, identity: &ApplicationIdentity) -> Result<()> {
    let mut private_key_seed_hex = identity.key.private_hex();
    let mut bytes = serde_json::to_vec_pretty(&IdentityDocument {
        service_id: &identity.service_id,
        boot_epoch: &identity.boot_epoch,
        application_key_id: &identity.application_key_id,
        private_key_seed_hex: &private_key_seed_hex,
    })?;
    bytes.push(b'\n');
    let result = write_new_private(path, &bytes);
    bytes.zeroize();
    private_key_seed_hex.zeroize();
    result
}

fn write_installer_identity(path: &Path, key_id: &str, key: &GeneratedKey) -> Result<()> {
    let mut private_key_seed_hex = key.private_hex();
    let mut public_key_hex = key.public_hex();
    let mut bytes = serde_json::to_vec_pretty(&InstallerIdentity {
        schema: "bloom.installer-identity.1",
        key_id,
        private_key_seed_hex: &private_key_seed_hex,
        public_key_hex: &public_key_hex,
    })?;
    bytes.push(b'\n');
    let result = write_new_private(path, &bytes);
    bytes.zeroize();
    private_key_seed_hex.zeroize();
    public_key_hex.zeroize();
    result
}

fn sign_provenance_catalog(
    source: &Path,
    destination: &Path,
    installer_key_id: &str,
    installer: &GeneratedKey,
) -> Result<()> {
    let mut source_bytes = read_public_template(source)?;
    let mut catalog: ProvenanceCatalog =
        serde_json::from_slice(&source_bytes).context("parse unsigned provenance catalog")?;
    source_bytes.zeroize();
    catalog.validate_shape()?;
    let installer_key_id = Token::new(installer_key_id)?;
    for record in &mut catalog.records {
        record.installer_key_id = installer_key_id.clone();
        record.installer_signature = Base64UrlBytes::from_bytes(&[]);
        let mut message = PROVENANCE_RECORD_SIGNATURE_DOMAIN.to_vec();
        message.extend_from_slice(&serde_jcs::to_vec(&record)?);
        record.installer_signature =
            Base64UrlBytes::from_bytes(&installer.signing_key().sign(&message).to_bytes());
        message.zeroize();
    }
    let mut catalog_bytes = serde_json::to_vec_pretty(&catalog)?;
    catalog_bytes.push(b'\n');
    let result = write_new_private(destination, &catalog_bytes);
    catalog_bytes.zeroize();
    result
}

fn write_new_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", path.display()))
}

fn decimal_arg(value: &std::ffi::OsStr, name: &str) -> Result<u32> {
    let value = value
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("{name} is not UTF-8"))?;
    let parsed = value
        .parse::<u32>()
        .with_context(|| format!("{name} is not a decimal 32-bit ID"))?;
    if parsed == 0 || parsed.to_string() != value {
        bail!("{name} is not a canonical positive decimal ID");
    }
    Ok(parsed)
}

struct ApplicationIdentity {
    service_id: String,
    boot_epoch: String,
    application_key_id: String,
    key: GeneratedKey,
}

impl ApplicationIdentity {
    fn generate(service_id: &str, login_uid: u32) -> Self {
        let mut epoch = [0_u8; 16];
        OsRng.fill_bytes(&mut epoch);
        Self {
            service_id: service_id.to_owned(),
            boot_epoch: hex::encode(epoch),
            application_key_id: format!("{service_id}-app-{login_uid}"),
            key: GeneratedKey::new(),
        }
    }
}

struct GeneratedKey {
    seed: [u8; 32],
}

struct SecretReplacements(BTreeMap<&'static str, String>);

impl Drop for SecretReplacements {
    fn drop(&mut self) {
        for value in self.0.values_mut() {
            value.zeroize();
        }
    }
}

impl GeneratedKey {
    fn new() -> Self {
        let mut seed = [0_u8; 32];
        OsRng.fill_bytes(&mut seed);
        Self { seed }
    }

    fn signing_key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.seed)
    }

    fn private_hex(&self) -> String {
        hex::encode(self.seed)
    }

    fn public_hex(&self) -> String {
        hex::encode(self.signing_key().verifying_key().to_bytes())
    }
}

impl Drop for GeneratedKey {
    fn drop(&mut self) {
        self.seed.zeroize();
    }
}

#[derive(Serialize)]
struct IdentityDocument<'a> {
    service_id: &'a str,
    boot_epoch: &'a str,
    application_key_id: &'a str,
    private_key_seed_hex: &'a str,
}

#[derive(Serialize)]
struct InstallerIdentity<'a> {
    schema: &'static str,
    key_id: &'a str,
    private_key_seed_hex: &'a str,
    public_key_hex: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
    use std::os::unix::fs::PermissionsExt as _;

    fn template_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join("packaging/triad/macos/config")
    }

    #[test]
    fn generated_material_is_fresh_cross_pinned_and_provenance_signed() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output");
        fs::create_dir(&output).unwrap();
        fs::set_permissions(&output, fs::Permissions::from_mode(0o700)).unwrap();
        let plan = EnrollmentPlan {
            template_dir: template_dir(),
            output_dir: output.clone(),
            login_uid: 501,
            broker_uid: 250_501,
            signer_uid: 250_502,
            session_socket_gid: 260_501,
            release_digest: "11".repeat(32),
        };
        generate_for_owner(&plan, rustix::process::geteuid().as_raw()).unwrap();

        let edge: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("edge-manifest.json")).unwrap()).unwrap();
        let broker_identity: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("broker-identity.json")).unwrap())
                .unwrap();
        let broker_seed: [u8; 32] =
            hex::decode(broker_identity["private_key_seed_hex"].as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
        assert_eq!(
            edge["broker"]["application_public_key_hex"]
                .as_str()
                .unwrap(),
            hex::encode(
                SigningKey::from_bytes(&broker_seed)
                    .verifying_key()
                    .to_bytes()
            )
        );
        assert_eq!(edge["session_socket_gid"], 260_501);

        let installer: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("installer-identity.json")).unwrap())
                .unwrap();
        let public: [u8; 32] = hex::decode(installer["public_key_hex"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let verifier = VerifyingKey::from_bytes(&public).unwrap();
        let catalog: ProvenanceCatalog =
            serde_json::from_slice(&fs::read(output.join("provenance-catalog.json")).unwrap())
                .unwrap();
        for record in catalog.records {
            let mut unsigned = record.clone();
            let signature: [u8; 64] = unsigned.installer_signature.decode().try_into().unwrap();
            unsigned.installer_signature = Base64UrlBytes::from_bytes(&[]);
            let mut message = PROVENANCE_RECORD_SIGNATURE_DOMAIN.to_vec();
            message.extend_from_slice(&serde_jcs::to_vec(&unsigned).unwrap());
            verifier
                .verify(&message, &Signature::from_bytes(&signature))
                .unwrap();
        }
        for name in [
            "machine-identity.json",
            "broker-identity.json",
            "signer-identity.json",
            "revoke-identity.json",
            "session-identity.json",
            "installer-identity.json",
            "broker.json",
            "signer.json",
        ] {
            assert_eq!(
                fs::metadata(output.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
