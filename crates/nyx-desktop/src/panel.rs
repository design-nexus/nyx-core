//! Side panels that never show together (Control Center, Notification Center).
//!
//! Each panel registers `io.github.design_nexus.nyx.Panel1` at [`PATH`] on its own
//! application's bus connection:
//!
//! - `Show()`, `Hide()`, `Toggle()`, `IsOpen() -> b`, for keybinds and bar widgets
//!   (`gdbus call --session --dest io.github.design_nexus.nyx.ControlCenter
//!   --object-path /io/github/design_nexus/nyx/Panel
//!   --method io.github.design_nexus.nyx.Panel1.Toggle`);
//! - the signal `Opened(s id)`, broadcast whenever a panel opens.
//!
//! Every panel listens for `Opened` and hides when another panel opened. There is no
//! coordinator: a panel that isn't running, or crashed, just isn't heard from.

use anyhow::{Context, Result};
use gio::prelude::*;
use std::rc::Rc;

pub const INTERFACE: &str = "io.github.design_nexus.nyx.Panel1";
pub const PATH: &str = "/io/github/design_nexus/nyx/Panel";

const XML: &str = r#"<node>
  <interface name="io.github.design_nexus.nyx.Panel1">
    <method name="Show"/>
    <method name="Hide"/>
    <method name="Toggle"/>
    <method name="IsOpen"><arg type="b" name="open" direction="out"/></method>
    <signal name="Opened"><arg type="s" name="id"/></signal>
  </interface>
</node>"#;

/// What the panel does when asked. All run on the main thread.
pub struct Handlers {
    pub show: Box<dyn Fn()>,
    pub hide: Box<dyn Fn()>,
    pub is_open: Box<dyn Fn() -> bool>,
}

/// A registered panel. Keep it for as long as the panel exists; dropping it stops
/// listening for other panels (the D-Bus object stays until the connection closes).
pub struct Panel {
    inner: Rc<Inner>,
    _subscription: gio::SignalSubscription,
}

struct Inner {
    id: String,
    connection: gio::DBusConnection,
    handlers: Handlers,
}

impl Inner {
    fn show(&self) {
        (self.handlers.show)();
        self.announce();
    }

    fn announce(&self) {
        let _ = self.connection.emit_signal(None, PATH, INTERFACE, "Opened", Some(&(self.id.as_str(),).to_variant()));
    }
}

impl Panel {
    /// Register panel `id` (`"control-center"`, `"notification-center"`) on
    /// `connection` (a `gio::Application`'s `dbus_connection()`, after startup).
    pub fn serve(connection: &gio::DBusConnection, id: &str, handlers: Handlers) -> Result<Panel> {
        let info = gio::DBusNodeInfo::for_xml(XML)?.lookup_interface(INTERFACE).context("Panel1 interface")?;
        let inner = Rc::new(Inner { id: id.to_string(), connection: connection.clone(), handlers });
        let calls = Rc::downgrade(&inner);
        connection
            .register_object(PATH, &info)
            .method_call(move |_, _, _, _, method, _, invocation| {
                let Some(inner) = calls.upgrade() else {
                    invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "panel is gone");
                    return;
                };
                match method {
                    "Show" => inner.show(),
                    "Hide" => (inner.handlers.hide)(),
                    "Toggle" => {
                        if (inner.handlers.is_open)() {
                            (inner.handlers.hide)()
                        } else {
                            inner.show()
                        }
                    }
                    "IsOpen" => {
                        invocation.return_value(Some(&((inner.handlers.is_open)(),).to_variant()));
                        return;
                    }
                    _ => {}
                }
                invocation.return_value(None);
            })
            .build()?;
        let others = Rc::downgrade(&inner);
        let subscription = connection.subscribe_to_signal(
            None,
            Some(INTERFACE),
            Some("Opened"),
            Some(PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let Some(inner) = others.upgrade() else { return };
                let opened = signal.parameters.get::<(String,)>().map(|(id,)| id);
                if opened.is_some_and(|id| id != inner.id) && (inner.handlers.is_open)() {
                    (inner.handlers.hide)();
                }
            },
        );
        Ok(Panel { inner, _subscription: subscription })
    }

    /// Tell the other panels this one opened. Call it when the panel opens some way
    /// other than this interface (its own window action, a click).
    pub fn announce(&self) {
        self.inner.announce();
    }
}
