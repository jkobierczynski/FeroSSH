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
    // Non-blocking: a live listener with a full backlog must not hold up
    // the forwarding thread. Only a refusal says nothing listens.
    if probe.set_nonblocking(true).is_err() {
        return;
    }
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

/// What tells one file from another at the same path on Unix: device,
/// inode and birth time where the file system keeps one (an inode freed
/// by a delete can be reused at once, but not with the same birth time).
/// Not the change time: a chmod of the socket changes that.
#[cfg(unix)]
fn file_id(path: &str) -> Option<(u64, u64, Option<std::time::SystemTime>)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some((meta.dev(), meta.ino(), meta.created().ok()))
}

/// The socket file itself (a reparse point), held open with only the
/// right to delete it and shared in every way, so others can still
/// connect, rename or delete it.
#[cfg(windows)]
fn open_for_delete(path: &str) -> Option<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    std::fs::OpenOptions::new()
        .access_mode(DELETE.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path)
        .ok()
}

/// Deletes the very file `file` is a handle to, wherever it is now.
#[cfg(windows)]
#[allow(unsafe_code)]
fn delete_by_handle(file: &std::fs::File) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO,
        FILE_DISPOSITION_INFO_EX, FILE_DISPOSITION_INFO_EX_FLAGS, FileDispositionInfo,
        FileDispositionInfoEx, SetFileInformationByHandle,
    };
    let handle = HANDLE(file.as_raw_handle());
    // POSIX semantics free the name at once, even while someone else
    // still has the file open, so the path can be bound again.
    let posix = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_INFO_EX_FLAGS(
            FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0,
        ),
    };
    // SAFETY: a handle the File owns for the call, and our own structs
    // with their exact sizes.
    let done = unsafe {
        SetFileInformationByHandle(
            handle,
            FileDispositionInfoEx,
            std::ptr::from_ref(&posix).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if done.is_err() {
        // Older Windows or a file system without POSIX deletes.
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: as above.
        let _ = unsafe {
            SetFileInformationByHandle(
                handle,
                FileDispositionInfo,
                std::ptr::from_ref(&info).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
    }
}

/// Removes the socket file of a listener when dropped, as the listener's
/// task is when its forward stops: only the file it made, never one
/// something else put at that path since.
///
/// On Windows it keeps a handle to the file it made and deletes through
/// that handle, so a replacement at the path is never touched. Unix has
/// no handle to a socket file, so it compares device, inode and birth
/// time just before removing.
pub struct Unlink {
    #[cfg(unix)]
    path: String,
    #[cfg(unix)]
    id: Option<(u64, u64, Option<std::time::SystemTime>)>,
    #[cfg(windows)]
    file: Option<std::fs::File>,
}

impl Unlink {
    /// For the socket just bound at `path`.
    pub fn new(path: String) -> Self {
        #[cfg(unix)]
        {
            let id = file_id(&path);
            Self { path, id }
        }
        #[cfg(windows)]
        {
            Self {
                file: open_for_delete(&path),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Self {}
        }
    }

    /// Whether it knows its file well enough to remove it.
    #[cfg(test)]
    fn armed(&self) -> bool {
        #[cfg(unix)]
        return self.id.is_some();
        #[cfg(windows)]
        return self.file.is_some();
        #[cfg(not(any(unix, windows)))]
        return false;
    }
}

impl Drop for Unlink {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.id.is_some() && file_id(&self.path) == self.id && is_socket_file(&self.path) {
            let _ = std::fs::remove_file(&self.path);
        }
        #[cfg(windows)]
        if let Some(file) = &self.file {
            delete_by_handle(file);
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

/// Whether a `connect` error means the connection is under way (or the
/// listener's backlog is full, on Linux): try again shortly.
fn connect_pending(e: &io::Error) -> bool {
    // EALREADY and EINPROGRESS on Linux, WSAEALREADY and WSAEINPROGRESS
    // on Windows.
    e.kind() == io::ErrorKind::WouldBlock
        || e.kind() == io::ErrorKind::Interrupted
        || matches!(e.raw_os_error(), Some(114 | 115 | 10036 | 10037))
}

/// Whether a `connect` error means an earlier attempt already connected:
/// EISCONN on Linux, WSAEISCONN on Windows.
fn already_connected(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(106 | 10056))
}

/// Connects to the socket at `path`. Non-blocking all the way, so a
/// caller's timeout drops the socket and nothing is left waiting.
pub async fn connect(path: &str) -> io::Result<BoxSock> {
    let addr = SockAddr::unix(path)?;
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    socket.set_nonblocking(true)?;
    loop {
        match socket.connect(&addr) {
            Ok(()) => break,
            Err(e) if already_connected(&e) => break,
            Err(e) if connect_pending(&e) => tokio::time::sleep(ACCEPT_POLL).await,
            Err(e) => return Err(e),
        }
    }
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
        let unlink = Unlink::new(path.clone());
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

    #[tokio::test]
    async fn connecting_where_nothing_listens_fails() {
        let path = temp_path("nobody");
        let _ = std::fs::remove_file(&path);
        assert!(connect(&path).await.is_err());
    }
}

/// Runs on Windows too, where CI has AF_UNIX.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod identity_tests {
    use super::*;

    #[test]
    fn a_socket_put_in_its_place_is_left_alone() {
        let dir =
            std::env::temp_dir().join(format!("ferossh-sock-{}-replaced", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("s.sock").to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&path);
        let first = bind(&path).unwrap();
        let unlink = Unlink::new(path.clone());
        assert!(unlink.armed(), "no hold on the bound socket");
        drop(first);
        // Moved away, not removed, so its inode is not reused.
        let aside = format!("{path}.old");
        std::fs::rename(&path, &aside).unwrap();
        let second = bind(&path).unwrap();
        drop(unlink);
        assert!(is_socket_file(&path), "the replacement was removed");
        drop(second);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&aside);
    }

    #[test]
    fn its_own_socket_is_removed() {
        let dir = std::env::temp_dir().join(format!("ferossh-sock-{}-own", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("s.sock").to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&path);
        let listener = bind(&path).unwrap();
        let unlink = Unlink::new(path.clone());
        assert!(unlink.armed(), "no hold on the bound socket");
        drop(listener);
        drop(unlink);
        assert!(
            !std::path::Path::new(&path).exists(),
            "the socket file was left behind"
        );
    }

    /// Unix only: on Windows the guard's handle keeps the deleted name
    /// until the guard goes, so nothing can be bound there before it.
    #[cfg(unix)]
    #[test]
    fn a_socket_bound_after_a_delete_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("ferossh-sock-{}-rebound", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("s.sock").to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&path);
        let first = bind(&path).unwrap();
        let unlink = Unlink::new(path.clone());
        assert!(unlink.armed(), "no hold on the bound socket");
        drop(first);
        std::fs::remove_file(&path).unwrap();
        // Birth times come from a coarse clock: let it tick.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = bind(&path).unwrap();
        drop(unlink);
        assert!(is_socket_file(&path), "the replacement was removed");
        drop(second);
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn its_own_socket_is_removed_after_a_chmod() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ferossh-sock-{}-chmod", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("s.sock").to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&path);
        let listener = bind(&path).unwrap();
        let unlink = Unlink::new(path.clone());
        assert!(unlink.armed(), "no hold on the bound socket");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        drop(listener);
        drop(unlink);
        assert!(
            !std::path::Path::new(&path).exists(),
            "the socket file was left behind"
        );
    }
}
