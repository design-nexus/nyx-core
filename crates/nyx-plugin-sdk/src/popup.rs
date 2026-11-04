//! Open your own window under your item (`gtk` feature; third-party plugins need the
//! `windows` permission). Click events carry an [`Anchor`]: where the item is.

use crate::protocol::Anchor;
use gtk::gdk;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

/// Make `window` a layer-shell popup below the item `anchor` describes, kept on screen.
pub fn popup_at(window: &gtk::Window, anchor: &Anchor) {
    window.init_layer_shell();
    window.set_namespace(Some("nyx-plugin-popup"));
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::OnDemand);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Left, true);
    let monitor = gdk::Display::default().and_then(|d| {
        d.monitors().iter::<gdk::Monitor>().flatten().find(|m| m.connector().as_deref() == Some(anchor.monitor.as_str()))
    });
    let screen_w = monitor.as_ref().map(|m| m.geometry().width()).unwrap_or(i32::MAX);
    if let Some(m) = &monitor {
        window.set_monitor(Some(m));
    }
    let (w, _) = window.default_size();
    let x = anchor.x.min(screen_w - w.max(1) - 8).max(8);
    window.set_margin(Edge::Left, x);
    window.set_margin(Edge::Top, anchor.y + anchor.height + 4);
}
