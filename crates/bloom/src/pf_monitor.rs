//! One-shot root packet-filter monitor for the macOS Unix-principal profile.

use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, bail};
use rustix::{
    fs::{Gid, Uid, chown},
    process::geteuid,
};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

const STATUS_SCHEMA: &str = "bloom.macos-network-containment.1";

#[derive(Serialize)]
struct Status {
    schema: &'static str,
    login_uid: u32,
    build_digest: String,
    anchor_sha256: String,
    checked_at_unix_ms: u64,
    available: bool,
}

pub fn run_once() -> Result<()> {
    if geteuid() != Uid::ROOT {
        bail!("the packet-filter monitor must run as root");
    }
    if std::env::consts::OS != "macos" {
        bail!("the packet-filter monitor requires macOS");
    }
    let enrollment_root = Path::new("/Library/Application Support/BloomTriad/enrollments");
    require_directory(enrollment_root, 0o755)?;
    let pf_enabled = command_output("/sbin/pfctl", &["-s", "info"])
        .is_ok_and(|output| output.contains("Status: Enabled"));
    let checked_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time precedes the Unix epoch")?
        .as_millis()
        .try_into()
        .context("system time does not fit u64 milliseconds")?;

    let mut enrollments = fs::read_dir(enrollment_root)
        .context("read Bloom enrollment root")?
        .collect::<std::io::Result<Vec<_>>>()
        .context("enumerate Bloom enrollments")?;
    enrollments.sort_by_key(|entry| entry.file_name());
    let mut all_available = true;
    for entry in enrollments {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        require_file(&path, 0o644)?;
        let enrollment: serde_json::Value = serde_json::from_slice(
            &fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("decode {}", path.display()))?;
        let login_uid = required_u32(&enrollment, "login_uid")?;
        if path.file_name().and_then(|value| value.to_str()) != Some(&format!("{login_uid}.json")) {
            bail!("enrollment filename does not match its login UID");
        }
        let broker_uid = required_u32(&enrollment, "broker_uid")?;
        let signer_uid = required_u32(&enrollment, "signer_uid")?;
        let build_digest = required_digest(&enrollment, "release_digest")?;
        let anchor = PathBuf::from(format!("/etc/pf.anchors/com.bloom.triad.{login_uid}"));
        require_file_mode(&anchor, 0o600)?;
        let anchor_bytes =
            fs::read(&anchor).with_context(|| format!("read {}", anchor.display()))?;
        let anchor_sha256 = hex::encode(Sha256::digest(&anchor_bytes));
        let loaded = command_output(
            "/sbin/pfctl",
            &["-a", &format!("com.bloom.triad/{login_uid}"), "-sr"],
        );
        let available = pf_enabled
            && loaded.as_ref().is_ok_and(|rules| {
                rules.contains("block")
                    && (rules.contains(&broker_uid.to_string())
                        || rules.contains(&format!("bloom-broker-{login_uid}")))
                    && (rules.contains(&signer_uid.to_string())
                        || rules.contains(&format!("bloom-signer-{login_uid}")))
            });
        all_available &= available;
        let status = Status {
            schema: STATUS_SCHEMA,
            login_uid,
            build_digest,
            anchor_sha256,
            checked_at_unix_ms,
            available,
        };
        write_status(login_uid, &status)?;
    }
    if !all_available {
        bail!("one or more Bloom packet-filter anchors is unavailable");
    }
    Ok(())
}

fn write_status(login_uid: u32, status: &Status) -> Result<()> {
    let directory = PathBuf::from(format!("/private/var/run/bloom/{login_uid}/containment"));
    require_directory(&directory, 0o755)?;
    let destination = directory.join("status.json");
    match fs::symlink_metadata(&destination) {
        Ok(_) => require_file(&destination, 0o644)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect containment status"),
    }
    let temporary = directory.join(format!("status.json.new.{}", std::process::id()));
    let result = (|| {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .context("create containment status")?;
        let bytes = serde_json::to_vec(status).context("encode containment status")?;
        output
            .write_all(&bytes)
            .context("write containment status")?;
        output.write_all(b"\n").context("terminate status record")?;
        output.sync_all().context("sync containment status")?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o644))
            .context("set containment status mode")?;
        chown(&temporary, Some(Uid::ROOT), Some(Gid::ROOT))
            .context("set containment status ownership")?;
        fs::rename(&temporary, &destination).context("publish containment status")?;
        fs::File::open(&directory)
            .context("open containment status directory")?
            .sync_all()
            .context("sync containment status directory")
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn command_output(program: &str, arguments: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .with_context(|| format!("execute {program}"))?;
    if !output.status.success() {
        bail!("{program} exited unsuccessfully");
    }
    String::from_utf8(output.stdout).context("packet-filter output is not UTF-8")
}

fn required_u32(value: &serde_json::Value, field: &str) -> Result<u32> {
    value
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| value.try_into().ok())
        .filter(|value| *value != 0)
        .with_context(|| format!("enrollment field {field} is not a positive u32"))
}

fn required_digest(value: &serde_json::Value, field: &str) -> Result<String> {
    let digest = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .with_context(|| format!("enrollment field {field} is not a string"))?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("enrollment field {field} is not a lowercase SHA-256 digest");
    }
    Ok(digest.to_owned())
}

fn require_directory(path: &Path, mode: u32) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.mode() & 0o7777 != mode
    {
        bail!(
            "unsafe root packet-filter monitor directory {}",
            path.display()
        );
    }
    Ok(())
}

fn require_file(path: &Path, mode: u32) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.mode() & 0o7777 != mode
        || metadata.nlink() != 1
    {
        bail!("unsafe root packet-filter monitor file {}", path.display());
    }
    Ok(())
}

fn require_file_mode(path: &Path, mode: u32) -> Result<()> {
    require_file(path, mode)
}
