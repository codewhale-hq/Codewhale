use std::net::{IpAddr, SocketAddr, TcpListener as StdTcpListener};
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, ensure};
use parking_lot::Mutex;
use tokio::net::TcpListener;

type CapturedListener = Result<Option<Mutex<Option<StdTcpListener>>>, String>;

static INHERITED_LISTENER: OnceLock<CapturedListener> = OnceLock::new();

fn captured() -> Result<Option<&'static Mutex<Option<StdTcpListener>>>> {
    match INHERITED_LISTENER.get_or_init(|| capture().map_err(|error| format!("{error:#}"))) {
        Ok(None) if std::env::var_os("CODEWHALE_RUNTIME_LISTEN_FD").is_some() => Err(anyhow!(
            "Inherited Runtime listener must be supplied at process startup"
        )),
        Ok(listener) => Ok(listener.as_ref()),
        Err(error) => Err(anyhow!(error.clone())),
    }
}

fn capture() -> Result<Option<Mutex<Option<StdTcpListener>>>> {
    let Some(marker) = std::env::var_os("CODEWHALE_RUNTIME_LISTEN_FD") else {
        return Ok(None);
    };
    ensure!(
        marker == "3",
        "CODEWHALE_RUNTIME_LISTEN_FD must be exactly 3"
    );
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;

        let flags = unsafe { libc::fcntl(3, libc::F_GETFD) };
        ensure!(
            flags >= 0,
            "Inherited Runtime listener descriptor is unavailable"
        );
        ensure!(
            unsafe { libc::fcntl(3, libc::F_SETFD, flags | libc::FD_CLOEXEC) } >= 0,
            "Failed to isolate the inherited Runtime listener from child processes"
        );
        let listener = unsafe { StdTcpListener::from_raw_fd(3) };
        ensure!(
            socket_option(libc::SOL_SOCKET, libc::SO_TYPE)? == libc::SOCK_STREAM && is_listening()?,
            "Inherited Runtime descriptor must be a listening TCP socket"
        );
        socket_option(libc::IPPROTO_TCP, libc::TCP_NODELAY)
            .context("Inherited Runtime listener is not TCP")?;
        ensure!(
            socket_option(libc::SOL_SOCKET, libc::SO_REUSEPORT)? == 0,
            "Inherited Runtime listener must not permit port sharing"
        );
        ensure!(
            listener.local_addr()?.ip().is_loopback(),
            "Inherited Runtime listener must be bound to loopback"
        );
        Ok(Some(Mutex::new(Some(listener))))
    }
    #[cfg(not(unix))]
    anyhow::bail!("Inherited Runtime listeners are supported only on Unix")
}

#[cfg(all(unix, target_vendor = "apple"))]
fn is_listening() -> Result<bool> {
    const TCP_STATE_LISTEN: u8 = 1;
    let mut state: u8 = 0;
    let mut length = std::mem::size_of_val(&state) as libc::socklen_t;
    let status = unsafe {
        libc::getsockopt(
            3,
            libc::IPPROTO_TCP,
            libc::TCP_CONNECTION_INFO,
            std::ptr::from_mut(&mut state).cast(),
            &mut length,
        )
    };
    ensure!(
        status == 0 && length as usize == std::mem::size_of_val(&state),
        "Failed to validate inherited Runtime listener TCP state"
    );
    Ok(state == TCP_STATE_LISTEN)
}

#[cfg(all(unix, not(target_vendor = "apple")))]
fn is_listening() -> Result<bool> {
    Ok(socket_option(libc::SOL_SOCKET, libc::SO_ACCEPTCONN)? == 1)
}

#[cfg(unix)]
fn socket_option(level: libc::c_int, name: libc::c_int) -> Result<libc::c_int> {
    let mut value: libc::c_int = 0;
    let mut length = std::mem::size_of_val(&value) as libc::socklen_t;
    let status = unsafe {
        libc::getsockopt(
            3,
            level,
            name,
            std::ptr::from_mut(&mut value).cast(),
            &mut length,
        )
    };
    ensure!(
        status == 0 && length as usize == std::mem::size_of_val(&value),
        "Failed to validate inherited Runtime listener socket options"
    );
    Ok(value)
}

pub(crate) fn requested() -> Result<bool> {
    Ok(captured()?.is_some())
}

pub(crate) fn discard() {
    if let Some(Ok(Some(listener))) = INHERITED_LISTENER.get() {
        drop(listener.lock().take());
    }
}

pub(super) fn take(host: &str, port: u16, insecure_no_auth: bool) -> Result<Option<TcpListener>> {
    let Some(captured) = captured()? else {
        return Ok(None);
    };
    let listener = captured
        .lock()
        .take()
        .context("Inherited Runtime listener has already been consumed or rejected")?;
    let ip: IpAddr = host
        .parse()
        .context("Inherited Runtime listener requires a numeric host")?;
    ensure!(
        ip.is_loopback() && port != 0 && !insecure_no_auth,
        "Inherited Runtime listener requires authenticated HTTP on a nonzero loopback port"
    );
    let expected = SocketAddr::new(ip, port);
    ensure!(
        listener.local_addr()? == expected,
        "Inherited Runtime listener address does not match the requested host and port"
    );
    listener.set_nonblocking(true)?;
    Ok(Some(TcpListener::from_std(listener)?))
}
