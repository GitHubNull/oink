//! XDND atoms, protocol messages and target discovery. All windows here are
//! override-redirect InputOnly: invisible, they never take focus and never
//! intercept real input, but GDK's DnD window cache still finds them.
use crate::linux::x11::Conn;
use std::io;

pub(super) const XDND_VERSION: u32 = 5;
pub(super) const MAX_INLINE_TYPES: usize = 3;

pub(super) struct Xdnd {
    pub(super) selection: u64,
    pub(super) aware: u64,
    pub(super) enter: u64,
    pub(super) position: u64,
    pub(super) status: u64,
    pub(super) leave: u64,
    pub(super) drop: u64,
    pub(super) finished: u64,
    pub(super) type_list: u64,
    pub(super) action_copy: u64,
    pub(super) proxy: u64,
}

impl Xdnd {
    pub(super) fn load(conn: &Conn) -> io::Result<Self> {
        let atoms = conn.intern(&[
            "XdndSelection",
            "XdndAware",
            "XdndEnter",
            "XdndPosition",
            "XdndStatus",
            "XdndLeave",
            "XdndDrop",
            "XdndFinished",
            "XdndTypeList",
            "XdndActionCopy",
            "XdndProxy",
        ])?;
        Ok(Self {
            selection: atoms[0],
            aware: atoms[1],
            enter: atoms[2],
            position: atoms[3],
            status: atoms[4],
            leave: atoms[5],
            drop: atoms[6],
            finished: atoms[7],
            type_list: atoms[8],
            action_copy: atoms[9],
            proxy: atoms[10],
        })
    }
}

/// The XDND version a window advertises, when it is a usable one (>= 3).
pub(super) fn aware_version(conn: &Conn, xdnd: &Xdnd, window: u64) -> Option<u32> {
    let version = conn.read_value32_keep(window, xdnd.aware).ok().flatten()?;
    (3..=XDND_VERSION).contains(&version).then_some(version)
}

/// The window a drag should talk to: the deepest XdndAware ancestor of the
/// window under the pointer, honoring XdndProxy.
pub(super) fn find_dest(conn: &Conn, xdnd: &Xdnd, skip: &[u64]) -> Option<(u64, u32)> {
    let mut window = conn.pointer_leaf(skip)?;
    // Walk up from the leaf to the XdndAware client window.
    let mut depth = 0;
    while depth < 32 {
        if let Some(version) = aware_version(conn, xdnd, window) {
            let dest = proxy_dest(conn, xdnd, window).unwrap_or(window);
            return Some((dest, version));
        }
        window = conn.parent_of(window)?;
        depth += 1;
    }
    None
}

fn proxy_dest(conn: &Conn, xdnd: &Xdnd, window: u64) -> Option<u64> {
    let proxy = u64::from(conn.read_value32_keep(window, xdnd.proxy).ok().flatten()?);
    (proxy != 0 && aware_version(conn, xdnd, proxy).is_some()).then_some(proxy)
}

pub(super) fn enter(source: u64, version: u32, types: &[u64], more: bool) -> [u32; 5] {
    let mut data = [0u32; 5];
    data[0] = source as u32;
    data[1] = (version << 24) | u32::from(more);
    for (index, kind) in types.iter().take(MAX_INLINE_TYPES).enumerate() {
        data[2 + index] = *kind as u32;
    }
    data
}

pub(super) fn position(source: u64, x: i32, y: i32, timestamp: u32, action: u64) -> [u32; 5] {
    let mut data = [0u32; 5];
    data[0] = source as u32;
    data[2] = pack_point(x, y);
    data[3] = timestamp;
    data[4] = action as u32;
    data
}

pub(super) fn status(target: u64, accept: bool, want_position: bool, action: u64) -> [u32; 5] {
    let mut data = [0u32; 5];
    data[0] = target as u32;
    data[1] = u32::from(accept) | (u32::from(want_position) << 1);
    data[4] = action as u32;
    data
}

pub(super) fn leave(source: u64) -> [u32; 5] {
    let mut data = [0u32; 5];
    data[0] = source as u32;
    data
}

pub(super) fn drop(source: u64, timestamp: u32) -> [u32; 5] {
    let mut data = [0u32; 5];
    data[0] = source as u32;
    data[2] = timestamp;
    data
}

pub(super) fn finished(target: u64, accept: bool, action: u64) -> [u32; 5] {
    let mut data = [0u32; 5];
    data[0] = target as u32;
    // bit1 marks a version 5 target that reads the performed action.
    data[1] = u32::from(accept) | 2;
    data[4] = action as u32;
    data
}

pub(super) fn pack_point(x: i32, y: i32) -> u32 {
    ((x as u16 as u32) << 16) | y as u16 as u32
}

#[cfg(test)]
pub(super) fn unpack_point(packed: u32) -> (i32, i32) {
    let x = (packed >> 16) as u16 as i16;
    let y = (packed & 0xffff) as u16 as i16;
    (i32::from(x), i32::from(y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_round_trip_through_the_wire_packing() {
        for (x, y) in [(0, 0), (1920, 1080), (37, 42), (-5, 7), (32767, -32768)] {
            assert_eq!(unpack_point(pack_point(x, y)), (x, y));
        }
    }

    #[test]
    fn enter_packs_the_version_and_type_list_flag() {
        let data = enter(0x1234, 5, &[7, 8, 9], true);
        assert_eq!(data[0], 0x1234);
        assert_eq!(data[1], (5 << 24) | 1);
        assert_eq!(&data[2..5], &[7, 8, 9]);
        let short = enter(1, 5, &[7], false);
        assert_eq!(short[1], 5 << 24);
        assert_eq!(&short[3..5], &[0, 0]);
    }

    #[test]
    fn status_and_finished_flags() {
        assert_eq!(status(1, true, true, 2)[1], 3);
        assert_eq!(status(1, false, true, 2)[1], 2);
        assert_eq!(finished(1, true, 2)[1], 3);
        assert_eq!(finished(1, false, 2)[1], 2);
    }
}
