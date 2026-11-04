//! Shared-memory frames. A plugin granted `frames` connects to a Unix socket the host
//! listens on (its path is in `hello`). For each `frame_request` the host makes two
//! `memfd` buffers (ARGB8888, premultiplied, `width × height × 4` bytes at the
//! monitor's scale), maps them read-only, and sends them over the socket with one JSON
//! line: `{"item","node","width","height","stride","buffers":2}`. The plugin draws
//! into one, sends `frame_ready` with its index, and draws the next into the other.

use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A read-only mapping of a plugin's buffer.
pub struct Mapping {
    ptr: *mut libc::c_void,
    len: usize,
}

// The host only reads; the plugin writing concurrently at worst tears one frame.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    pub fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr as *const u8, self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr, self.len);
        }
    }
}

#[derive(Clone)]
pub struct Buffers {
    pub item: String,
    pub node: String,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub maps: Arc<[Mapping; 2]>,
}

impl std::fmt::Debug for Buffers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Buffers({}/{} {}x{})", self.item, self.node, self.width, self.height)
    }
}

/// The socket for one plugin run.
pub struct Server {
    pub path: PathBuf,
    stream: Arc<Mutex<Option<UnixStream>>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        if let Some(s) = self.stream.lock().unwrap().take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Server {
    pub fn listen(path: &Path) -> std::io::Result<Server> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        let stream: Arc<Mutex<Option<UnixStream>>> = Arc::default();
        let s = stream.clone();
        std::thread::Builder::new().name("nyx-plugin-frames".into()).spawn(move || {
            // One client: the plugin. A later connection replaces an earlier one.
            for conn in listener.incoming().flatten() {
                *s.lock().unwrap() = Some(conn);
            }
        })?;
        Ok(Server { path: path.to_path_buf(), stream })
    }

    /// Make and send buffers. Fails when the plugin hasn't connected.
    pub fn allocate(&self, item: &str, node: &str, width: i32, height: i32) -> std::io::Result<Buffers> {
        let stride = width * 4;
        let len = (stride * height).max(4) as usize;
        let mut fds = vec![];
        let mut maps = vec![];
        for i in 0..2 {
            let name = std::ffi::CString::new(format!("nyx-frame-{i}")).unwrap();
            let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            if unsafe { libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, fd.as_raw_fd(), 0) };
            if ptr == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            maps.push(Mapping { ptr, len });
            fds.push(fd);
        }
        let guard = self.stream.lock().unwrap();
        let Some(stream) = guard.as_ref() else {
            return Err(std::io::Error::new(std::io::ErrorKind::NotConnected, "the plugin hasn't connected to the frame socket"));
        };
        let header = format!(
            "{}\n",
            serde_json::json!({"item": item, "node": node, "width": width, "height": height, "stride": stride, "buffers": 2})
        );
        send_fds(stream, header.as_bytes(), &[fds[0].as_raw_fd(), fds[1].as_raw_fd()])?;
        let mut maps = maps.into_iter();
        let maps = [maps.next().unwrap(), maps.next().unwrap()];
        Ok(Buffers { item: item.into(), node: node.into(), width, height, stride, maps: Arc::new(maps) })
    }
}

/// Write `data` with file descriptors attached (SCM_RIGHTS).
pub fn send_fds(stream: &UnixStream, data: &[u8], fds: &[RawFd]) -> std::io::Result<()> {
    let mut iov = libc::iovec { iov_base: data.as_ptr() as *mut _, iov_len: data.len() };
    let fd_bytes = std::mem::size_of_val(fds);
    let space = unsafe { libc::CMSG_SPACE(fd_bytes as u32) } as usize;
    let mut cbuf = vec![0u8; space];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cbuf.as_mut_ptr() as *mut _;
    msg.msg_controllen = space as _;
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(fd_bytes as u32) as _;
        std::ptr::copy_nonoverlapping(fds.as_ptr() as *const u8, libc::CMSG_DATA(cmsg), fd_bytes);
        if libc::sendmsg(stream.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    let _ = (&*stream).flush();
    Ok(())
}

/// Read one message and the file descriptors that came with it (for plugins: the SDK).
pub fn recv_fds(stream: &UnixStream, buf: &mut [u8], max_fds: usize) -> std::io::Result<(usize, Vec<OwnedFd>)> {
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr() as *mut _, iov_len: buf.len() };
    let space = unsafe { libc::CMSG_SPACE((max_fds * std::mem::size_of::<RawFd>()) as u32) } as usize;
    let mut cbuf = vec![0u8; space];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cbuf.as_mut_ptr() as *mut _;
    msg.msg_controllen = space as _;
    let n = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut fds = vec![];
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<RawFd>();
                let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                for i in 0..count {
                    fds.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(data.add(i))));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    Ok((n as usize, fds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_reach_the_plugin_and_back() {
        let t = tempfile::tempdir().unwrap();
        let server = Server::listen(&t.path().join("frames.sock")).unwrap();
        let client = UnixStream::connect(&server.path).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let bufs = server.allocate("viz", "f", 8, 2).unwrap();
        let mut head = [0u8; 256];
        let (n, fds) = recv_fds(&client, &mut head, 2).unwrap();
        assert_eq!(fds.len(), 2);
        let header: serde_json::Value = serde_json::from_slice(&head[..n - 1]).unwrap();
        assert_eq!(header["stride"], 32);
        // The plugin writes into buffer 1; the host sees it.
        let len = 8 * 2 * 4;
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_WRITE, libc::MAP_SHARED, fds[1].as_raw_fd(), 0) };
        assert_ne!(ptr, libc::MAP_FAILED);
        unsafe { std::ptr::write_bytes(ptr as *mut u8, 0xab, len) };
        assert!(bufs.maps[1].bytes().iter().all(|b| *b == 0xab));
        assert!(bufs.maps[0].bytes().iter().all(|b| *b == 0));
        unsafe { libc::munmap(ptr, len) };
    }
}
