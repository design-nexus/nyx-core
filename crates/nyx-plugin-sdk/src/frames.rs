//! Draw pixels yourself and show them in an item (needs the `frames` permission).
//!
//! ```no_run
//! # use nyx_plugin_sdk::{Plugin, frames::FrameSurface};
//! # let (plugin, _) = Plugin::<()>::start();
//! let mut surface = FrameSurface::open(&plugin, "viz", "f", 120, 24).unwrap();
//! surface.draw(|pixels, w, h, stride| { /* BGRA, premultiplied */ });
//! ```

use crate::Plugin;
use crate::protocol::PluginMsg;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

struct Map {
    ptr: *mut libc::c_void,
    len: usize,
    _fd: OwnedFd,
}

impl Drop for Map {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr, self.len) };
    }
}

pub struct FrameSurface<T: Send + 'static> {
    plugin: Plugin<T>,
    item: String,
    node: String,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    maps: Vec<Map>,
    next: usize,
    _socket: UnixStream,
}

fn recv_fds(stream: &UnixStream, buf: &mut [u8]) -> std::io::Result<(usize, Vec<OwnedFd>)> {
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr() as *mut _, iov_len: buf.len() };
    let space = unsafe { libc::CMSG_SPACE((4 * std::mem::size_of::<RawFd>()) as u32) } as usize;
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
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let count = ((*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<RawFd>();
                let data = libc::CMSG_DATA(c) as *const RawFd;
                for i in 0..count {
                    fds.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(data.add(i))));
                }
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    Ok((n as usize, fds))
}

impl<T: Send + 'static> FrameSurface<T> {
    /// Connect to the host's frame socket and get two `width × height` buffers for
    /// the `frame` node `node` of `item`. Sizes are in pixels: multiply logical sizes
    /// by the monitor's scale for sharp frames.
    pub fn open(plugin: &Plugin<T>, item: &str, node: &str, width: i32, height: i32) -> std::io::Result<FrameSurface<T>> {
        let path = plugin
            .hello
            .frame_socket
            .clone()
            .ok_or_else(|| std::io::Error::other("no frame socket: the plugin needs the `frames` permission"))?;
        let socket = UnixStream::connect(path)?;
        std::thread::sleep(std::time::Duration::from_millis(20));
        plugin.send(&PluginMsg::FrameRequest { item: item.into(), node: node.into(), width, height });
        let mut head = [0u8; 512];
        let (n, fds) = recv_fds(&socket, &mut head)?;
        let header: serde_json::Value = serde_json::from_slice(head[..n].trim_ascii_end()).map_err(std::io::Error::other)?;
        let stride = header["stride"].as_i64().unwrap_or((width * 4) as i64) as i32;
        let len = (stride * height) as usize;
        let mut maps = vec![];
        for fd in fds {
            let ptr = unsafe {
                libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd.as_raw_fd(), 0)
            };
            if ptr == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            maps.push(Map { ptr, len, _fd: fd });
        }
        if maps.len() != 2 {
            return Err(std::io::Error::other("the host sent the wrong number of buffers"));
        }
        Ok(FrameSurface { plugin: plugin.clone(), item: item.into(), node: node.into(), width, height, stride, maps, next: 0, _socket: socket })
    }

    /// Draw into the next buffer (BGRA, premultiplied alpha) and show it.
    pub fn draw(&mut self, f: impl FnOnce(&mut [u8], i32, i32, i32)) {
        let m = &self.maps[self.next];
        let pixels = unsafe { std::slice::from_raw_parts_mut(m.ptr as *mut u8, m.len) };
        f(pixels, self.width, self.height, self.stride);
        self.plugin.send(&PluginMsg::FrameReady { item: self.item.clone(), node: self.node.clone(), buffer: self.next as u8 });
        self.next = 1 - self.next;
    }
}
