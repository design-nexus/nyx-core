//! Window thumbnails through the standard capture protocols (`capture` feature):
//! `ext-foreign-toplevel-list` names each window (its identifier is Hyprland's
//! `stableId`), `ext-foreign-toplevel-image-capture-source` makes it a capture source,
//! and `ext-image-copy-capture` copies its last frame into shared memory. This works for
//! windows on hidden workspaces too.
//!
//! [`capture_blocking`] opens its own Wayland connection for one batch of windows and
//! closes it when done, so nothing is held (or has to keep reading events) between
//! batches. Frames are scaled down on the calling thread. With the `glib` feature,
//! [`capture`] runs a batch on a worker thread and hands each [`Shot`] to the main loop.

use std::io::{Read, Seek};
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicU64, Ordering};
use wayland_client::protocol::{wl_buffer, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1 as handle, ext_foreign_toplevel_list_v1 as list,
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1 as sources, ext_image_capture_source_v1 as source,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1 as frame, ext_image_copy_capture_manager_v1 as capture,
    ext_image_copy_capture_session_v1 as session,
};

/// One window's thumbnail, ready for GDK.
pub struct Shot {
    pub stable_id: String,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub opaque: bool,
}

impl Shot {
    /// Bytes per row (the data is tightly packed, 4 bytes a pixel, BGRA in memory).
    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }
}

/// Capture `ids` (stable ids) on a worker thread, each scaled to fit `max_w` × `max_h`
/// pixels; `on_shot` runs on the main loop for each one that succeeds.
#[cfg(feature = "glib")]
pub fn capture(ids: Vec<String>, max_w: u32, max_h: u32, on_shot: impl Fn(Shot) + 'static) {
    let (tx, rx) = async_channel::unbounded::<Shot>();
    std::thread::Builder::new()
        .name("nyx-desktop-capture".into())
        .spawn(move || {
            if let Err(e) = capture_blocking(&ids, max_w, max_h, |shot| {
                let _ = tx.send_blocking(shot);
            }) {
                eprintln!("nyx-desktop: window thumbnails: {e}");
            }
        })
        .ok();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(shot) = rx.recv().await {
            on_shot(shot);
        }
    });
}

#[derive(Default)]
struct Toplevel {
    handle: Option<handle::ExtForeignToplevelHandleV1>,
    id: String,
}

#[derive(Default)]
struct State {
    list: Option<list::ExtForeignToplevelListV1>,
    sources: Option<sources::ExtForeignToplevelImageCaptureSourceManagerV1>,
    capture: Option<capture::ExtImageCopyCaptureManagerV1>,
    shm: Option<wl_shm::WlShm>,
    toplevels: Vec<Toplevel>,
    size: (u32, u32),
    format: Option<wl_shm::Format>,
    constraints_done: bool,
    stopped: bool,
    result: Option<bool>,
}

/// Capture `ids` (stable ids) on this thread, each scaled to fit `max_w` × `max_h`
/// pixels, calling `send` for each one that succeeds. Windows that are gone are skipped.
pub fn capture_blocking(ids: &[String], max_w: u32, max_h: u32, send: impl Fn(Shot)) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
    let mut queue: EventQueue<State> = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut state = State::default();
    // Globals, then the toplevel list, then each toplevel's identifier.
    for _ in 0..3 {
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    }
    let (Some(sources), Some(manager), Some(shm)) = (state.sources.clone(), state.capture.clone(), state.shm.clone()) else {
        return Err("the compositor doesn't offer window capture".into());
    };
    for id in ids {
        let Some(h) = state.toplevels.iter().find(|t| &t.id == id).and_then(|t| t.handle.clone()) else { continue };
        if let Some(shot) = one(&mut queue, &qh, &mut state, &sources, &manager, &shm, &h, id, max_w, max_h) {
            send(shot);
        }
    }
    if let Some(l) = &state.list {
        l.stop();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn one(
    queue: &mut EventQueue<State>,
    qh: &QueueHandle<State>,
    state: &mut State,
    sources: &sources::ExtForeignToplevelImageCaptureSourceManagerV1,
    manager: &capture::ExtImageCopyCaptureManagerV1,
    shm: &wl_shm::WlShm,
    h: &handle::ExtForeignToplevelHandleV1,
    id: &str,
    max_w: u32,
    max_h: u32,
) -> Option<Shot> {
    state.size = (0, 0);
    state.format = None;
    state.constraints_done = false;
    state.stopped = false;
    state.result = None;
    let src = sources.create_source(h, qh, ());
    let sess = manager.create_session(&src, capture::Options::empty(), qh, ());
    while !state.constraints_done && !state.stopped {
        queue.blocking_dispatch(state).ok()?;
    }
    let (w, ht) = state.size;
    let format = state.format?;
    if state.stopped || w == 0 || ht == 0 {
        sess.destroy();
        src.destroy();
        return None;
    }
    let stride = w * 4;
    let len = (stride * ht) as usize;
    // Shared memory: a file in the runtime folder, unlinked at once.
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = dir.join(format!("nyx-shot-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let mut file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    file.set_len(len as u64).ok()?;
    let pool = shm.create_pool(file.as_fd(), len as i32, qh, ());
    let buffer = pool.create_buffer(0, w as i32, ht as i32, stride as i32, format, qh, ());
    let fr = sess.create_frame(qh, ());
    fr.attach_buffer(&buffer);
    fr.damage_buffer(0, 0, w as i32, ht as i32);
    fr.capture();
    while state.result.is_none() && !state.stopped {
        if queue.blocking_dispatch(state).is_err() {
            break;
        }
    }
    let ok = state.result == Some(true);
    fr.destroy();
    sess.destroy();
    src.destroy();
    buffer.destroy();
    pool.destroy();
    if !ok {
        return None;
    }
    let mut data = vec![0u8; len];
    file.seek(std::io::SeekFrom::Start(0)).ok()?;
    file.read_exact(&mut data).ok()?;
    let (data, tw, th) = shrink(&data, w, ht, max_w, max_h);
    Some(Shot { stable_id: id.to_string(), width: tw, height: th, data, opaque: format == wl_shm::Format::Xrgb8888 })
}

/// Scale a 4-byte-per-pixel image down to fit `max_w` × `max_h`, averaging each block
/// of source pixels (a box filter: smooth enough for a thumbnail, and cheap).
pub fn shrink(data: &[u8], w: u32, h: u32, max_w: u32, max_h: u32) -> (Vec<u8>, u32, u32) {
    let scale = (w as f64 / max_w as f64).max(h as f64 / max_h as f64).max(1.0);
    let (tw, th) = (((w as f64 / scale).round() as u32).max(1), ((h as f64 / scale).round() as u32).max(1));
    if (tw, th) == (w, h) {
        return (data.to_vec(), w, h);
    }
    let mut out = vec![0u8; (tw * th * 4) as usize];
    for ty in 0..th {
        let y0 = (ty as u64 * h as u64 / th as u64) as u32;
        let y1 = (((ty + 1) as u64 * h as u64 / th as u64) as u32).max(y0 + 1).min(h);
        for tx in 0..tw {
            let x0 = (tx as u64 * w as u64 / tw as u64) as u32;
            let x1 = (((tx + 1) as u64 * w as u64 / tw as u64) as u32).max(x0 + 1).min(w);
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                let row = (y * w * 4) as usize;
                for x in x0..x1 {
                    let i = row + (x * 4) as usize;
                    for c in 0..4 {
                        sum[c] += data[i + c] as u32;
                    }
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            let o = ((ty * tw + tx) * 4) as usize;
            for c in 0..4 {
                out[o + c] = (sum[c] / n) as u8;
            }
        }
    }
    (out, tw, th)
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(s: &mut Self, r: &wl_registry::WlRegistry, e: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = e {
            match interface.as_str() {
                "ext_foreign_toplevel_list_v1" => s.list = Some(r.bind(name, version.min(1), qh, ())),
                "ext_foreign_toplevel_image_capture_source_manager_v1" => s.sources = Some(r.bind(name, 1, qh, ())),
                "ext_image_copy_capture_manager_v1" => s.capture = Some(r.bind(name, 1, qh, ())),
                "wl_shm" => s.shm = Some(r.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<list::ExtForeignToplevelListV1, ()> for State {
    fn event(s: &mut Self, _: &list::ExtForeignToplevelListV1, e: list::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let list::Event::Toplevel { toplevel } = e {
            s.toplevels.push(Toplevel { handle: Some(toplevel), id: String::new() });
        }
    }
    wayland_client::event_created_child!(State, list::ExtForeignToplevelListV1, [
        list::EVT_TOPLEVEL_OPCODE => (handle::ExtForeignToplevelHandleV1, ())
    ]);
}

impl Dispatch<handle::ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        s: &mut Self,
        h: &handle::ExtForeignToplevelHandleV1,
        e: handle::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let handle::Event::Identifier { identifier } = e
            && let Some(t) = s.toplevels.iter_mut().find(|t| t.handle.as_ref() == Some(h))
        {
            t.id = identifier;
        }
    }
}

impl Dispatch<session::ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(
        s: &mut Self,
        _: &session::ExtImageCopyCaptureSessionV1,
        e: session::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            session::Event::BufferSize { width, height } => s.size = (width, height),
            // Prefer a format with alpha; take what's offered otherwise.
            session::Event::ShmFormat { format: WEnum::Value(f) } => {
                if f == wl_shm::Format::Argb8888 || (s.format.is_none() && f == wl_shm::Format::Xrgb8888) {
                    s.format = Some(f);
                }
            }
            session::Event::Done => s.constraints_done = true,
            session::Event::Stopped => s.stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<frame::ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        s: &mut Self,
        _: &frame::ExtImageCopyCaptureFrameV1,
        e: frame::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            frame::Event::Ready => s.result = Some(true),
            frame::Event::Failed { .. } => s.result = Some(false),
            _ => {}
        }
    }
}

delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: sources::ExtForeignToplevelImageCaptureSourceManagerV1);
delegate_noop!(State: capture::ExtImageCopyCaptureManagerV1);
delegate_noop!(State: source::ExtImageCaptureSourceV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrink_keeps_the_aspect_and_averages() {
        // 4×2 image: left half white, right half black.
        let mut data = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                let v = if x < 2 { 255 } else { 0 };
                data.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let (out, w, h) = shrink(&data, 4, 2, 2, 2);
        assert_eq!((w, h), (2, 1));
        assert_eq!(&out[0..4], &[255, 255, 255, 255]);
        assert_eq!(&out[4..8], &[0, 0, 0, 255]);
        let (same, w, h) = shrink(&data, 4, 2, 100, 100);
        assert_eq!((w, h), (4, 2), "never scaled up");
        assert_eq!(same, data);
    }

    #[test]
    fn shrink_fits_the_tighter_side() {
        let data = vec![0u8; 2560 * 1558 * 4];
        let (_, w, h) = shrink(&data, 2560, 1558, 424, 256);
        assert!(w <= 424 && h <= 256);
        assert_eq!(w, 421);
    }
}
