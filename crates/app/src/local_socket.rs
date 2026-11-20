//! Unix sockets on this PC, for forwards that listen on one
//! (`-L C:\path\db.sock:db:5432`) or connect to one
//! (`-R /tmp/app.sock:C:\path\app.sock`).
//!
//! Windows has had `AF_UNIX` stream sockets since Windows 10 1803; tokio
//! does not wrap them there, so the sockets come from socket2. An
//! accepted or connected socket becomes a tokio stream: a `UnixStream`
//! on Unix, and on Windows a `TcpStream`, which only reads and writes
//! through the socket and never looks at its address family. Listening
//! polls a non-blocking socket instead, because tokio's listener reads
//! every peer's address as an IP address.
//!
//! Who may connect is up to the socket file's permissions, which it takes
//! from its folder: the peer check of TCP listeners (`peer_owner`) does
//! not apply.

use std::io;
use std::path::Path;
use std::time::Duration;

use socket2::{Domain, SockAddr, Socket, Type};
use tokio::io::{AsyncRead, AsyncWrite};

/// A stream either kind of socket gives.
pub trait Sock: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> Sock for T {}

pub type BoxSock = Box<dyn Sock>;

/// How often a listener looks for a new connection.
const ACCEPT_POLL: Duration = Duration::from_millis(20);

/// A socket path on this PC must be absolute: relative to what FeroSSH
/// happened to start in would surprise.
pub fn check_path(path: &str) -> Result<(), String> {
    if Path::new(path).is_absolute() {
        Ok(())
    } else if cfg!(windows) {
        Err(format!(
            "{path} is not a full path on this PC; give one with a drive letter, in brackets: \
             [C:\\Users\\me\\app.sock]."
        ))
    } else {
        Err(format!("{path} is not a full path."))
    }
}

/// Whether the file at `path` is a socket.
fn is_socket_file(path: &str) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        meta.file_type().is_socket()
    }
    #[cfg(windows)]
    {
        // An AF_UNIX socket file is a reparse point that is neither a
        // link nor a folder.
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            && !meta.file_type().is_symlink()
            && !meta.is_dir()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = meta;
        false
    }
}

/// Removes a socket file nothing listens on any more, as one left behind
/// when FeroSSH ended without cleaning up. A live socket, or a file that
/// is not a socket, stays, and listening there fails.
fn remove_stale(path: &str) {
    if !is_socket_file(path) {
        return;
    }
    let Ok(addr) = SockAddr::unix(path) else {
        return;
    };
    let Ok(probe) = Socket::new(Domain::UNIX, Type::STREAM, None) else {
        return;
    };
    if matches!(probe.connect(&addr), Err(e) if e.kind() == io::ErrorKind::ConnectionRefused) {
        let _ = std::fs::remove_file(path);
    }
}

/// Listens on `path`.
pub fn bind(path: &str) -> io::Result<Socket> {
    remove_stale(path);
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    socket.bind(&SockAddr::unix(path)?)?;
    socket.listen(128)?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

/// Removes the socket file of a listener when dropped, as the listener's
/// task is when its forward stops.
pub struct Unlink(pub String);

impl Drop for Unlink {
    fn drop(&mut self) {
        if is_socket_file(&self.0) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// The next connection to `listener`.
pub async fn accept(listener: &Socket) -> io::Result<BoxSock> {
    loop {
        match listener.accept() {
            Ok((socket, _)) => return into_tokio(socket),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                tokio::time::sleep(ACCEPT_POLL).await;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// Connects to the socket at `path`.
pub async fn connect(path: &str) -> io::Result<BoxSock> {
    let addr = SockAddr::unix(path)?;
    let socket = tokio::task::spawn_blocking(move || {
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        socket.connect(&addr)?;
        Ok::<_, io::Error>(socket)
    })
    .await
    .map_err(io::Error::other)??;
    into_tokio(socket)
}

fn into_tokio(socket: Socket) -> io::Result<BoxSock> {
    socket.set_nonblocking(true)?;
    #[cfg(unix)]
    {
        let s: std::os::unix::net::UnixStream = socket.into();
        Ok(Box::new(tokio::net::UnixStream::from_std(s)?))
    }
    #[cfg(not(unix))]
    {
        let s: std::net::TcpStream = socket.into();
        Ok(Box::new(tokio::net::TcpStream::from_std(s)?))
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temp_path(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("ferossh-sock-{}-{name}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("s.sock").to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn listens_accepts_connects_and_cleans_up() {
        let path = temp_path("roundtrip");
        let listener = bind(&path).unwrap();
        let unlink = Unlink(path.clone());
        let server = async {
            let mut s = accept(&listener).await.unwrap();
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).await.unwrap();
            s.write_all(&buf).await.unwrap();
        };
        let client = async {
            let mut c = connect(&path).await.unwrap();
            c.write_all(b"hello").await.unwrap();
            let mut buf = [0u8; 5];
            c.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"hello");
        };
        tokio::join!(server, client);
        drop(listener);
        drop(unlink);
        assert!(!Path::new(&path).exists());
    }

    #[test]
    fn a_stale_socket_is_replaced_but_a_live_one_or_a_file_is_not() {
        let path = temp_path("stale");
        let first = bind(&path).unwrap();
        // Live: listening again fails.
        assert!(bind(&path).is_err());
        drop(first);
        // Left behind: replaced.
        assert!(is_socket_file(&path));
        let second = bind(&path).unwrap();
        drop(second);
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"not a socket").unwrap();
        assert!(bind(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not a socket");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn relative_paths_are_refused() {
        assert!(check_path("/tmp/x.sock").is_ok());
        assert!(check_path("x/y.sock").is_err());
    }
}
