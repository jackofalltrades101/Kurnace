//! Local core transport. Unix uses a 0600 Unix domain socket; Windows uses a
//! loopback TCP socket with its port recorded in `~/.kurnace/core.port`.
//! Both expose the same Read/Write connection type used by the CLI and app.
#[cfg(windows)]
use anyhow::Context;
use anyhow::{Result, bail};
use std::path::Path;

#[cfg(unix)]
pub type Conn = std::os::unix::net::UnixStream;
#[cfg(unix)]
pub type Listener = std::os::unix::net::UnixListener;

#[cfg(windows)]
pub type Conn = std::net::TcpStream;
#[cfg(windows)]
pub type Listener = std::net::TcpListener;

#[cfg(unix)]
pub fn connect(path: &Path) -> Result<Conn> {
    Ok(Conn::connect(path)?)
}

#[cfg(windows)]
pub fn connect(path: &Path) -> Result<Conn> {
    let txt = std::fs::read_to_string(path)
        .with_context(|| format!("core not running (no {})", path.display()))?;
    let port: u16 = txt.trim().parse().context("invalid core.port")?;
    Ok(Conn::connect(("127.0.0.1", port))?)
}

#[cfg(unix)]
pub fn bind(path: &Path) -> Result<Listener> {
    if path.exists() {
        if connect(path).is_ok() {
            bail!("a core is already running on {}", path.display());
        }
        std::fs::remove_file(path)?;
    }
    let l = Listener::bind(path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

#[cfg(windows)]
pub fn bind(path: &Path) -> Result<Listener> {
    if path.exists() {
        if connect(path).is_ok() {
            bail!("a core is already running on {}", path.display());
        }
        let _ = std::fs::remove_file(path);
    }
    let l = Listener::bind(("127.0.0.1", 0))?;
    let port = l.local_addr()?.port();
    std::fs::write(path, port.to_string())?;
    Ok(l)
}

/// Remove the socket/port file on shutdown.
pub fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
}
