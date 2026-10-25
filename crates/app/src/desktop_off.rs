//! Stands in for the remote desktop pane when FeroSSH is built without
//! the `desktop-viewer` feature: [`Desktop`] has no values, so the
//! window's desktop paths compile to nothing and no viewer code ships.

use ferossh_render::Image;

/// Never made; see the module docs.
#[derive(Debug)]
pub enum Desktop {}

/// Never made either, without a [`Desktop`].
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
pub enum Update {
    Redraw,
    Title(String),
    Bell,
    AskPassword,
    Ended(String),
}

impl Desktop {
    pub fn poll(&mut self) -> Vec<Update> {
        match *self {}
    }

    pub fn running(&self) -> bool {
        match *self {}
    }

    pub fn asking_password(&self) -> bool {
        match *self {}
    }

    pub fn image(&self, _key: u64) -> Option<Image<'_>> {
        match *self {}
    }

    pub fn drawn(&mut self) {
        match *self {}
    }

    pub fn key(&mut self, _vk: u16, _extended: bool, _scan: u16, _down: bool) -> bool {
        match *self {}
    }

    pub fn char(&mut self, _c: char) -> bool {
        match *self {}
    }

    pub fn mouse(
        &mut self,
        _area: (i32, i32, i32, i32),
        _x: i32,
        _y: i32,
        _buttons: Option<(u8, bool)>,
    ) {
        match *self {}
    }

    pub fn wheel(&mut self, _area: (i32, i32, i32, i32), _x: i32, _y: i32, _notches: i32) {
        match *self {}
    }

    pub fn focus_lost(&mut self) {
        match *self {}
    }
}
