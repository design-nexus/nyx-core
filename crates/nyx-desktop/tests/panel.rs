//! Panel coordination over a private D-Bus. Its own test binary: the private
//! `dbus-daemon` must not run alongside the watcher tests' fake sockets.
#![cfg(feature = "glib")]

use nyx_desktop::panel::{Handlers, INTERFACE, PATH, Panel};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn connect(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(
        address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
        gio::Cancellable::NONE,
    )
    .unwrap()
}

fn panel(conn: &gio::DBusConnection, id: &str) -> (Panel, Rc<Cell<bool>>) {
    let open = Rc::new(Cell::new(false));
    let (a, b, c) = (open.clone(), open.clone(), open.clone());
    let p = Panel::serve(
        conn,
        id,
        Handlers {
            show: Box::new(move || a.set(true)),
            hide: Box::new(move || b.set(false)),
            is_open: Box::new(move || c.get()),
        },
    )
    .unwrap();
    (p, open)
}

fn call(from: &gio::DBusConnection, to: &gio::DBusConnection, method: &str) {
    from.call(
        to.unique_name().as_deref(),
        PATH,
        INTERFACE,
        method,
        None,
        None,
        gio::DBusCallFlags::NONE,
        1000,
        gio::Cancellable::NONE,
        |_| {},
    );
}

fn settle(until: impl Fn() -> bool) {
    let ctx = glib::MainContext::default();
    let start = Instant::now();
    while !until() && start.elapsed() < Duration::from_secs(3) {
        ctx.iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
    // Let anything else already sent arrive too.
    let end = Instant::now() + Duration::from_millis(50);
    while Instant::now() < end {
        ctx.iteration(false);
    }
}

#[test]
fn opening_one_panel_closes_the_other() {
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let address = bus.bus_address().unwrap();
    let (cc_conn, nc_conn, bar) = (connect(&address), connect(&address), connect(&address));
    let (_cc, cc_open) = panel(&cc_conn, "control-center");
    let (nc, nc_open) = panel(&nc_conn, "notification-center");

    call(&bar, &cc_conn, "Toggle");
    settle(|| cc_open.get());
    assert!(cc_open.get() && !nc_open.get());

    call(&bar, &nc_conn, "Show");
    settle(|| nc_open.get() && !cc_open.get());
    assert!(nc_open.get(), "the notification center opened");
    assert!(!cc_open.get(), "and the control center closed");

    // Opening from the panel's own UI announces too.
    cc_open.set(true);
    _cc.announce();
    settle(|| !nc_open.get());
    assert!(!nc_open.get());

    call(&bar, &cc_conn, "Toggle");
    settle(|| !cc_open.get());
    assert!(!cc_open.get(), "toggle closes an open panel");
    drop(nc);
    bus.down();
}
