//! Host secrets: the host key, MCP tokens. Never stored in SQLite.
//!
//! Backends, chosen once at first start and remembered:
//! - macOS: the login Keychain (Security framework);
//! - Linux with a Secret Service (GNOME Keyring, KWallet): via `secret-tool`,
//!   secrets passed on stdin, never on the command line;
//! - headless Linux: one owner-only file per secret under `data/secrets/`.
//!
//! This settles the spec's open question for milestone 1: Secret Service if
//! present, else an owner-only key file.

use anyhow::{anyhow, bail, Context, Result};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const SERVICE: &str = "agenthost";

pub trait SecretStore: Send + Sync {
    fn backend(&self) -> &'static str;
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>>;
    fn set(&self, name: &str, value: &[u8]) -> Result<()>;
    fn delete(&self, name: &str) -> Result<()>;
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        || name.starts_with('.')
    {
        bail!("invalid secret name {name:?}");
    }
    Ok(())
}

/// Owner-only files: directory 0700, files 0600.
pub struct FileSecrets {
    dir: PathBuf,
}

impl FileSecrets {
    pub fn new(dir: PathBuf) -> Result<Self> {
        crate::config::create_private_dir(&dir)?;
        Ok(Self { dir })
    }
}

impl SecretStore for FileSecrets {
    fn backend(&self) -> &'static str {
        "file"
    }
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        check_name(name)?;
        let p = self.dir.join(name);
        match std::fs::read(&p) {
            Ok(v) => {
                let mode = std::fs::metadata(&p)?.permissions().mode() & 0o077;
                if mode != 0 {
                    bail!("{} is readable by others; refusing to use it", p.display());
                }
                Ok(Some(v))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn set(&self, name: &str, value: &[u8]) -> Result<()> {
        check_name(name)?;
        let tmp = self.dir.join(format!(".{name}.tmp"));
        {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(value)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, self.dir.join(name))?;
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<()> {
        check_name(name)?;
        match std::fs::remove_file(self.dir.join(name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

/// Linux Secret Service through `secret-tool`.
pub struct SecretService {
    host_id: String,
}

impl SecretService {
    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<std::process::Output> {
        let mut child = Command::new("secret-tool")
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("run secret-tool")?;
        if let Some(input) = stdin {
            child.stdin.take().unwrap().write_all(input)?;
        }
        // secret-tool can hang without a session bus; bound it.
        let start = std::time::Instant::now();
        loop {
            if child.try_wait()?.is_some() {
                return Ok(child.wait_with_output()?);
            }
            if start.elapsed() > std::time::Duration::from_secs(5) {
                let _ = child.kill();
                bail!("secret-tool timed out");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Whether a Secret Service answers a store/lookup round trip.
    pub fn probe(host_id: &str) -> Option<SecretService> {
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none()
            && std::env::var_os("XDG_RUNTIME_DIR").is_none()
        {
            return None;
        }
        let s = SecretService {
            host_id: host_id.to_string(),
        };
        let ok =
            s.set("probe", b"ok").is_ok() && matches!(s.get("probe"), Ok(Some(v)) if v == b"ok");
        let _ = s.delete("probe");
        ok.then_some(s)
    }
}

impl SecretStore for SecretService {
    fn backend(&self) -> &'static str {
        "secret-service"
    }
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        check_name(name)?;
        let o = self.run(
            &[
                "lookup",
                "service",
                SERVICE,
                "host",
                &self.host_id,
                "name",
                name,
            ],
            None,
        )?;
        if o.status.success() && !o.stdout.is_empty() {
            let b64 = String::from_utf8_lossy(&o.stdout).trim().to_string();
            use base64::Engine;
            return Ok(Some(
                base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| anyhow!("corrupt secret: {e}"))?,
            ));
        }
        Ok(None)
    }
    fn set(&self, name: &str, value: &[u8]) -> Result<()> {
        check_name(name)?;
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(value);
        let label = format!("agenthost {name}");
        let o = self.run(
            &[
                "store",
                "--label",
                &label,
                "service",
                SERVICE,
                "host",
                &self.host_id,
                "name",
                name,
            ],
            Some(b64.as_bytes()),
        )?;
        if !o.status.success() {
            bail!(
                "secret-tool store failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<()> {
        check_name(name)?;
        let _ = self.run(
            &[
                "clear",
                "service",
                SERVICE,
                "host",
                &self.host_id,
                "name",
                name,
            ],
            None,
        )?;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub struct Keychain {
    host_id: String,
}

#[cfg(target_os = "macos")]
impl SecretStore for Keychain {
    fn backend(&self) -> &'static str {
        "keychain"
    }
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        check_name(name)?;
        let account = format!("{}:{name}", self.host_id);
        match security_framework::passwords::get_generic_password(SERVICE, &account) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.code() == -25300 => Ok(None),
            Err(e) => Err(anyhow!("keychain: {e}")),
        }
    }
    fn set(&self, name: &str, value: &[u8]) -> Result<()> {
        check_name(name)?;
        let account = format!("{}:{name}", self.host_id);
        security_framework::passwords::set_generic_password(SERVICE, &account, value)
            .map_err(|e| anyhow!("keychain: {e}"))
    }
    fn delete(&self, name: &str) -> Result<()> {
        check_name(name)?;
        let account = format!("{}:{name}", self.host_id);
        match security_framework::passwords::delete_generic_password(SERVICE, &account) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == -25300 => Ok(()),
            Err(e) => Err(anyhow!("keychain: {e}")),
        }
    }
}

/// Open the backend recorded for this host, choosing one on first start.
pub fn open(
    store: &crate::store::Store,
    data_dir: &std::path::Path,
    host_id: &str,
    force_file: bool,
) -> Result<Box<dyn SecretStore>> {
    let file = || -> Result<Box<dyn SecretStore>> {
        Ok(Box::new(FileSecrets::new(data_dir.join("secrets"))?))
    };
    let chosen = store.host_get("secret_backend")?;
    let backend: Box<dyn SecretStore> = match chosen.as_deref() {
        Some("file") => file()?,
        Some("secret-service") => Box::new(SecretService {
            host_id: host_id.to_string(),
        }),
        #[cfg(target_os = "macos")]
        Some("keychain") => Box::new(Keychain {
            host_id: host_id.to_string(),
        }),
        Some(other) => bail!("unknown secret backend {other} recorded for this host"),
        None => {
            let b: Box<dyn SecretStore> = if force_file {
                file()?
            } else {
                #[cfg(target_os = "macos")]
                {
                    Box::new(Keychain {
                        host_id: host_id.to_string(),
                    })
                }
                #[cfg(not(target_os = "macos"))]
                {
                    match SecretService::probe(host_id) {
                        Some(s) => Box::new(s),
                        None => file()?,
                    }
                }
            };
            store.host_set("secret_backend", b.backend())?;
            tracing::info!(backend = b.backend(), "secret backend chosen");
            b
        }
    };
    Ok(backend)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_backend_roundtrip_and_perms() {
        let d = tempfile::tempdir().unwrap();
        let s = FileSecrets::new(d.path().join("secrets")).unwrap();
        assert_eq!(s.get("k").unwrap(), None);
        s.set("k", b"v").unwrap();
        assert_eq!(s.get("k").unwrap().unwrap(), b"v");
        let mode = std::fs::metadata(d.path().join("secrets/k"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert!(s.get("../x").is_err());
        s.delete("k").unwrap();
        assert_eq!(s.get("k").unwrap(), None);
    }
}
