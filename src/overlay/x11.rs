//! X11-only window policy that GTK4 deliberately leaves to native APIs.

use anyhow::Result;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt, PropMode,
    StackMode,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub struct Window {
    connection: RustConnection,
    root: u32,
    id: u32,
}

impl Window {
    pub fn new(id: u32) -> Result<Self> {
        let (connection, screen) = x11rb::connect(None)?;
        let root = connection.setup().roots[screen].root;
        let window = Self {
            connection,
            root,
            id,
        };
        let c = &window.connection;
        c.change_window_attributes(id, &ChangeWindowAttributesAux::new().override_redirect(1))?
            .check()?;
        // ICCCM InputHint=false: mapping this surface must never move keyboard focus.
        c.change_property32(
            PropMode::REPLACE,
            id,
            AtomEnum::WM_HINTS,
            AtomEnum::WM_HINTS,
            &[1, 0, 0, 0, 0, 0, 0, 0, 0],
        )?
        .check()?;
        let kind = window.atom("_NET_WM_WINDOW_TYPE")?;
        let notification = window.atom("_NET_WM_WINDOW_TYPE_NOTIFICATION")?;
        c.change_property32(PropMode::REPLACE, id, kind, AtomEnum::ATOM, &[notification])?
            .check()?;
        let state = window.atom("_NET_WM_STATE")?;
        let flags = [
            window.atom("_NET_WM_STATE_ABOVE")?,
            window.atom("_NET_WM_STATE_SKIP_TASKBAR")?,
            window.atom("_NET_WM_STATE_SKIP_PAGER")?,
        ];
        c.change_property32(PropMode::REPLACE, id, state, AtomEnum::ATOM, &flags)?
            .check()?;
        c.flush()?;
        Ok(window)
    }

    fn atom(&self, name: &str) -> Result<Atom> {
        Ok(self
            .connection
            .intern_atom(false, name.as_bytes())?
            .reply()?
            .atom)
    }

    pub fn pointer(&self) -> Result<(i32, i32)> {
        let pointer = self.connection.query_pointer(self.root)?.reply()?;
        Ok((pointer.root_x.into(), pointer.root_y.into()))
    }

    pub fn place(&self, x: i32, y: i32, width: u32, height: u32) -> Result<()> {
        self.connection
            .configure_window(
                self.id,
                &ConfigureWindowAux::new()
                    .x(x)
                    .y(y)
                    .width(width)
                    .height(height)
                    .stack_mode(StackMode::ABOVE),
            )?
            .check()?;
        self.connection.flush()?;
        Ok(())
    }
}
