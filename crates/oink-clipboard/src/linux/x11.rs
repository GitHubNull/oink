//! One Xlib connection per thread; nothing here is shared across threads.
//! Every operation is a bounded request/response over the ICCCM selection
//! protocol. Payload bytes stay in the process that owns them.
use crate::linux::sys::{Property, wait_readable, words};
use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::RawFd;
use std::ptr;
use std::sync::Once;
use std::time::{Duration, Instant};
pub(crate) use x11_dl::xlib::{XEvent, XSelectionRequestEvent};
use x11_dl::xlib::{ClientMessageData, Display, XSetWindowAttributes, Xlib};

pub(crate) const PROPERTY_CHANGE_MASK: i64 = 1 << 22;
pub(crate) const EVENT_TYPE_PROPERTY_NOTIFY: i32 = 28;
pub(crate) const EVENT_TYPE_SELECTION_CLEAR: i32 = 29;
pub(crate) const EVENT_TYPE_SELECTION_REQUEST: i32 = 30;
pub(crate) const EVENT_TYPE_SELECTION_NOTIFY: i32 = 31;
pub(crate) const EVENT_TYPE_CLIENT_MESSAGE: i32 = 33;
pub(crate) const EVENT_STATE_NEW_VALUE: i32 = 0;
pub(crate) const EVENT_STATE_DELETE: i32 = 1;
/// Public ICCCM mask bits for `XFixesSelectSelectionInput`.
pub(crate) const XFIXES_SET_SELECTION_OWNER: u64 = 1 << 0;
pub(crate) const XFIXES_SELECTION_WINDOW_DESTROY: u64 = 1 << 1;
pub(crate) const XFIXES_SELECTION_CLIENT_CLOSE: u64 = 1 << 2;
pub(crate) const XFIXES_SELECTION_NOTIFY: i32 = 0;
pub(crate) const XFIXES_SUBTYPE_SET_SELECTION_OWNER: i32 = 0;
pub(crate) const XFIXES_SUBTYPE_WINDOW_DESTROY: i32 = 1;
pub(crate) const XFIXES_SUBTYPE_CLIENT_CLOSE: i32 = 2;

const CW_EVENT_MASK: u64 = 1 << 11;
const CW_OVERRIDE_REDIRECT: u64 = 1 << 9;
const INPUT_ONLY: u32 = 2;
const PROP_MODE_REPLACE: i32 = 0;
const PROPERTY_TYPE_NONE: u64 = 0;

static INIT: Once = Once::new();

extern "C" fn benign_error(_: *mut Display, _: *mut x11_dl::xlib::XErrorEvent) -> i32 {
    // A vanished requestor window is an ordinary peer event, not a crash.
    0
}

fn init(xlib: &Xlib) {
    INIT.call_once(|| {
        // SAFETY: no arguments, once per process, before this module issues a request.
        unsafe {
            (xlib.XInitThreads)();
            (xlib.XSetErrorHandler)(Some(benign_error));
        }
    });
}

/// A connection owned by exactly one thread for its whole lifetime.
pub(crate) struct Conn {
    xlib: Xlib,
    dpy: *mut Display,
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conn").finish_non_exhaustive()
    }
}

impl Conn {
    pub(crate) fn open() -> io::Result<Self> {
        let xlib = Xlib::open().map_err(|error| io::Error::other(format!("libX11: {error}")))?;
        init(&xlib);
        // SAFETY: null selects $DISPLAY; the returned connection is owned here.
        let dpy = unsafe { (xlib.XOpenDisplay)(ptr::null()) };
        if dpy.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cannot open the X11 display",
            ));
        }
        Ok(Self { xlib, dpy })
    }

    /// An unmapped InputOnly window receives the events subscribed on it.
    pub(crate) fn create_window(&self, event_mask: i64) -> io::Result<u64> {
        let root = self.root_window();
        // SAFETY: InputOnly windows take CopyFromParent depth, a null visual and
        // exactly the CWEventMask value that is initialized below.
        let mut attrs: XSetWindowAttributes = unsafe { std::mem::zeroed() };
        attrs.event_mask = event_mask as _;
        // SAFETY: InputOnly windows take CopyFromParent depth, a null visual and
        // exactly the initialized CWEventMask attribute.
        let window = unsafe {
            (self.xlib.XCreateWindow)(
                self.dpy,
                root,
                0,
                0,
                1,
                1,
                0,
                0,
                INPUT_ONLY,
                ptr::null_mut(),
                CW_EVENT_MASK,
                &mut attrs,
            )
        };
        if window == 0 {
            return Err(io::Error::other("XCreateWindow failed"));
        }
        Ok(window)
    }

    /// An override-redirect InputOnly window never interacts with the WM.
    pub(crate) fn create_override_window(&self, event_mask: i64) -> io::Result<u64> {
        let root = self.root_window();
        // SAFETY: InputOnly windows take CopyFromParent depth, a null visual and
        // exactly the attributes initialized below.
        let mut attrs: XSetWindowAttributes = unsafe { std::mem::zeroed() };
        attrs.override_redirect = 1;
        attrs.event_mask = event_mask as _;
        // SAFETY: see the attribute initialization above.
        let window = unsafe {
            (self.xlib.XCreateWindow)(
                self.dpy,
                root,
                0,
                0,
                1,
                1,
                0,
                0,
                INPUT_ONLY,
                ptr::null_mut(),
                CW_EVENT_MASK | CW_OVERRIDE_REDIRECT,
                &mut attrs,
            )
        };
        if window == 0 {
            return Err(io::Error::other("XCreateWindow failed"));
        }
        Ok(window)
    }

    pub(crate) fn destroy_window(&self, window: u64) {
        // SAFETY: window was created on this connection.
        unsafe { (self.xlib.XDestroyWindow)(self.dpy, window) };
    }

    pub(crate) fn root_window(&self) -> u64 {
        // SAFETY: default screen and root window always exist for a live display.
        unsafe { (self.xlib.XDefaultRootWindow)(self.dpy) }
    }

    /// Root dimensions in pixels; the held catcher spans exactly this so the
    /// pointer can never leave it, without an oversized ConfigureNotify.
    pub(crate) fn root_size(&self) -> (u32, u32) {
        let root = self.root_window();
        let mut r_return = 0u64;
        let (mut x, mut y) = (0i32, 0i32);
        let (mut width, mut height) = (0u32, 0u32);
        let (mut border, mut depth) = (0u32, 0u32);
        // SAFETY: all out pointers are valid stack variables for the root id.
        unsafe {
            let _ = (self.xlib.XGetGeometry)(
                self.dpy,
                root,
                &mut r_return,
                &mut x,
                &mut y,
                &mut width,
                &mut height,
                &mut border,
                &mut depth,
            );
        }
        (width.max(1), height.max(1))
    }

    pub(crate) fn move_resize_window(&self, window: u64, x: i32, y: i32, width: u32, height: u32) {
        // SAFETY: window was created on this connection.
        unsafe { (self.xlib.XMoveResizeWindow)(self.dpy, window, x, y, width, height) };
    }

    pub(crate) fn map_raised(&self, window: u64) {
        // SAFETY: window was created on this connection.
        unsafe { (self.xlib.XMapRaised)(self.dpy, window) };
    }

    pub(crate) fn unmap_window(&self, window: u64) {
        // SAFETY: window was created on this connection.
        unsafe { (self.xlib.XUnmapWindow)(self.dpy, window) };
    }

    /// Root coordinates of the pointer; None when it is on another screen.
    pub(crate) fn query_pointer(&self) -> Option<(i32, i32)> {
        self.query_pointer_full().map(|(x, y, _)| (x, y))
    }

    /// Root coordinates plus the current button/key modifier mask. The mask
    /// reflects XTEST-injected button state, so drag loops can observe an
    /// injected release without a second event subscription.
    pub(crate) fn query_pointer_full(&self) -> Option<(i32, i32, u32)> {
        let root = self.root_window();
        let (mut root_return, mut child_return) = (0u64, 0u64);
        let (mut root_x, mut root_y, mut win_x, mut win_y) = (0i32, 0i32, 0i32, 0i32);
        let mut mask = 0u32;
        // SAFETY: all out pointers are valid stack variables.
        let ok = unsafe {
            (self.xlib.XQueryPointer)(
                self.dpy,
                root,
                &mut root_return,
                &mut child_return,
                &mut root_x,
                &mut root_y,
                &mut win_x,
                &mut win_y,
                &mut mask,
            )
        };
        if ok == 0 {
            return None;
        }
        Some((root_x, root_y, mask))
    }

    /// The immediate child of `window` under the pointer, descending until the
    /// leaf. Windows in `skip` (and their subtrees) end the walk.
    pub(crate) fn pointer_leaf(&self, skip: &[u64]) -> Option<u64> {
        let mut window = self.query_pointer_in(self.root_window(), skip)?;
        let mut depth = 0;
        while depth < 32 {
            let Some(child) = self.query_pointer_in(window, skip) else {
                break;
            };
            window = child;
            depth += 1;
        }
        Some(window)
    }

    fn query_pointer_in(&self, window: u64, skip: &[u64]) -> Option<u64> {
        let (mut root_return, mut child_return) = (0u64, 0u64);
        let (mut root_x, mut root_y, mut win_x, mut win_y) = (0i32, 0i32, 0i32, 0i32);
        let mut mask = 0u32;
        // SAFETY: all out pointers are valid stack variables.
        let ok = unsafe {
            (self.xlib.XQueryPointer)(
                self.dpy,
                window,
                &mut root_return,
                &mut child_return,
                &mut root_x,
                &mut root_y,
                &mut win_x,
                &mut win_y,
                &mut mask,
            )
        };
        if ok == 0 || child_return == 0 || skip.contains(&child_return) {
            return None;
        }
        Some(child_return)
    }

    /// The parent of a window, stopping before the root.
    pub(crate) fn parent_of(&self, window: u64) -> Option<u64> {
        let root = self.root_window();
        let (mut root_return, mut parent_return) = (0u64, 0u64);
        let mut children: *mut u64 = ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: all out pointers are valid stack variables; children is XFree'd.
        let ok = unsafe {
            (self.xlib.XQueryTree)(
                self.dpy,
                window,
                &mut root_return,
                &mut parent_return,
                &mut children,
                &mut count,
            )
        };
        if !children.is_null() {
            // SAFETY: children came from XQueryTree.
            unsafe { (self.xlib.XFree)(children.cast()) };
        }
        (ok != 0 && parent_return != 0 && parent_return != root).then_some(parent_return)
    }

    /// Children of a window in bottom-to-top stacking order.
    #[cfg(test)]
    pub(crate) fn query_tree(&self, window: u64) -> Vec<u64> {
        let (mut root_return, mut parent_return) = (0u64, 0u64);
        let mut children: *mut u64 = ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: all out pointers are valid stack variables; children is XFree'd.
        let ok = unsafe {
            (self.xlib.XQueryTree)(
                self.dpy,
                window,
                &mut root_return,
                &mut parent_return,
                &mut children,
                &mut count,
            )
        };
        if ok == 0 || children.is_null() {
            return Vec::new();
        }
        // SAFETY: the library returned `count` window ids.
        let ids = unsafe { std::slice::from_raw_parts(children, count as usize) }.to_vec();
        // SAFETY: children came from XQueryTree.
        unsafe { (self.xlib.XFree)(children.cast()) };
        ids
    }

    /// Send an XDND ClientMessage with five 32-bit data words to one window.
    pub(crate) fn send_client_message(&self, window: u64, type_: u64, data: [u32; 5]) {
        // SAFETY: XEvent is plain data; the client_message arm is fully
        // initialized below before the event is sent.
        let mut event: XEvent = unsafe { std::mem::zeroed() };
        {
            // SAFETY: the event type below selects the ClientMessage arm.
            let event = unsafe { &mut event.client_message };
            event.type_ = EVENT_TYPE_CLIENT_MESSAGE;
            event.window = window;
            event.message_type = type_;
            event.format = 32;
            event.data = ClientMessageData::new();
            for (index, value) in data.into_iter().enumerate() {
                event.data.set_long(index, i64::from(value));
            }
        }
        // SAFETY: marshalled event, no propagation mask, sent to one window.
        unsafe { (self.xlib.XSendEvent)(self.dpy, window, 0, 0, &mut event) };
    }

    /// Decode one received ClientMessage into its five 32-bit words.
    pub(crate) fn client_message_words(&self, event: &XEvent) -> Option<(u64, u64, [u32; 5])> {
        // SAFETY: the union is plain data; type_ is always initialized.
        if unsafe { event.type_ } != EVENT_TYPE_CLIENT_MESSAGE {
            return None;
        }
        // SAFETY: the event type selects the ClientMessage arm.
        let message = unsafe { &event.client_message };
        if message.format != 32 {
            return None;
        }
        let mut words = [0u32; 5];
        for (index, word) in words.iter_mut().enumerate() {
            *word = u32::try_from(message.data.get_long(index)).unwrap_or(0);
        }
        Some((message.window, message.message_type, words))
    }

    /// Read one 32-bit word without deleting the property; None when absent.
    pub(crate) fn read_value32_keep(&self, window: u64, property: u64) -> io::Result<Option<u32>> {
        Ok(self.read_values32_keep(window, property, 1)?.into_iter().next())
    }

    /// Read up to `limit` 32-bit words without deleting the property; an empty
    /// vector means the property is absent or has the wrong format.
    pub(crate) fn read_values32_keep(&self, window: u64, property: u64, limit: usize) -> io::Result<Vec<u32>> {
        let mut type_ = 0u64;
        let mut format = 0i32;
        let mut items = 0u64;
        let mut bytes_after = 0u64;
        let mut data: *mut u8 = ptr::null_mut();
        // SAFETY: all outputs are initialized; data is XFree'd after copying.
        let status = unsafe {
            (self.xlib.XGetWindowProperty)(
                self.dpy,
                window,
                property,
                0,
                limit.max(1) as _,
                0,
                PROPERTY_TYPE_NONE,
                &mut type_,
                &mut format,
                &mut items,
                &mut bytes_after,
                &mut data,
            )
        };
        let value = if status != 0 || type_ == 0 || format != 32 || data.is_null() {
            Vec::new()
        } else {
            let count = items.min(limit as u64) as usize;
            // SAFETY: the library returned `count` initialized longs.
            let words = unsafe { std::slice::from_raw_parts(data as *mut std::ffi::c_long, count) };
            words.iter().filter_map(|word| u32::try_from(*word).ok()).collect()
        };
        if !data.is_null() {
            // SAFETY: data came from XGetWindowProperty.
            unsafe { (self.xlib.XFree)(data.cast()) };
        }
        Ok(value)
    }

    pub(crate) fn atom(&self, name: &str, only_if_exists: bool) -> io::Result<u64> {
        let name = CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "atom name"))?;
        // SAFETY: NUL-terminated name outlives the request.
        let atom = unsafe { (self.xlib.XInternAtom)(self.dpy, name.as_ptr(), only_if_exists as _) };
        Ok(atom)
    }

    pub(crate) fn intern(&self, names: &[&str]) -> io::Result<Vec<u64>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let names: Vec<CString> = names
            .iter()
            .map(|name| {
                CString::new(*name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "atom name"))
            })
            .collect::<io::Result<_>>()?;
        let mut pointers: Vec<*mut i8> = names.iter().map(|name| name.as_ptr() as *mut i8).collect();
        let mut atoms = vec![0u64; names.len()];
        // SAFETY: pointers and the output array match the declared count.
        let ok = unsafe {
            (self.xlib.XInternAtoms)(
                self.dpy,
                pointers.as_mut_ptr(),
                names.len() as i32,
                0,
                atoms.as_mut_ptr(),
            )
        };
        if ok == 0 {
            return Err(io::Error::other("XInternAtoms failed"));
        }
        Ok(atoms)
    }

    pub(crate) fn atom_name(&self, atom: u64) -> Option<String> {
        if atom == 0 {
            return None;
        }
        // SAFETY: Xlib owns the returned string until XFree; it is copied out.
        unsafe {
            let name = (self.xlib.XGetAtomName)(self.dpy, atom);
            if name.is_null() {
                return None;
            }
            let text = CStr::from_ptr(name).to_string_lossy().into_owned();
            (self.xlib.XFree)(name.cast());
            Some(text)
        }
    }

    pub(crate) fn select_input(&self, window: u64, mask: i64) {
        // SAFETY: per-client masks may be added to foreign windows (ICCCM INCR).
        unsafe { (self.xlib.XSelectInput)(self.dpy, window, mask as _) };
    }

    /// Subscribe to XFixes selection events on our own window; returns the
    /// extension event base used to recognise the events.
    pub(crate) fn select_xfixes_input(&self, window: u64, selection: u64, mask: u64) -> io::Result<i32> {
        let xfixes = x11_dl::xfixes::Xlib::open()
            .map_err(|error| io::Error::new(io::ErrorKind::NotFound, format!("libXfixes: {error}")))?;
        let mut event_base = 0;
        let mut error_base = 0;
        // SAFETY: two writable ints; the display is open.
        let present = unsafe { (xfixes.XFixesQueryExtension)(self.dpy, &mut event_base, &mut error_base) };
        if present == 0 {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "XFixes is unavailable"));
        }
        // SAFETY: our window and an interned selection atom.
        unsafe { (xfixes.XFixesSelectSelectionInput)(self.dpy, window, selection, mask as _) };
        Ok(event_base)
    }

    pub(crate) fn change_property(&self, window: u64, property: u64, type_: u64, format: i32, bytes: &[u8]) {
        // SAFETY: the buffer length in `format` units is derived from `bytes`.
        unsafe {
            (self.xlib.XChangeProperty)(
                self.dpy,
                window,
                property,
                type_,
                format,
                PROP_MODE_REPLACE,
                bytes.as_ptr(),
                (bytes.len() / (format as usize / 8).max(1)) as i32,
            );
        }
    }

    pub(crate) fn change_property32(&self, window: u64, property: u64, type_: u64, values: &[u32]) {
        let values: Vec<i64> = values.iter().map(|value| i64::from(*value)).collect();
        // SAFETY: format 32 values are `long` in the client library's buffer.
        unsafe {
            (self.xlib.XChangeProperty)(
                self.dpy,
                window,
                property,
                type_,
                32,
                PROP_MODE_REPLACE,
                values.as_ptr().cast(),
                values.len() as i32,
            );
        }
    }

    pub(crate) fn delete_property(&self, window: u64, property: u64) {
        // SAFETY: deleting a missing property is a no-op.
        unsafe { (self.xlib.XDeleteProperty)(self.dpy, window, property) };
    }

    /// Read and delete an 8-bit or 32-bit property without assuming its type,
    /// so an INCR marker and its byte chunks share one path.
    pub(crate) fn read_any(&self, window: u64, property: u64, limit: usize) -> io::Result<Option<Property>> {
        let Some(prop) = self.query(window, property, limit.max(1) / 4 + 1) else {
            return Ok(None);
        };
        if prop.bytes_after > 0 {
            return Err(io::Error::new(io::ErrorKind::FileTooLarge, "property exceeds the request window"));
        }
        Ok(Some(prop))
    }

    /// Read and delete the beginning of a byte property, truncating a larger
    /// value. Used to classify uri-list metadata without fetching a file list.
    pub(crate) fn read_bytes_prefix(
        &self,
        window: u64,
        property: u64,
        limit: usize,
    ) -> io::Result<Option<(u64, Vec<u8>)>> {
        let Some(prop) = self.query(window, property, limit.max(1) / 4 + 1) else {
            return Ok(None);
        };
        if prop.format != 8 {
            return Ok(None);
        }
        let mut bytes = prop.bytes;
        bytes.truncate(limit);
        Ok(Some((prop.type_, bytes)))
    }

    /// Read a 32-bit atom pair list, e.g. an ICCCM MULTIPLE request.
    pub(crate) fn read_pairs(
        &self,
        window: u64,
        property: u64,
        max_pairs: usize,
    ) -> io::Result<Option<Vec<(u64, u64)>>> {
        let Some(prop) = self.query(window, property, max_pairs.saturating_mul(2).max(2)) else {
            return Ok(None);
        };
        if prop.format != 32 || prop.bytes_after > 0 || !prop.items.is_multiple_of(2) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "property is not an atom pair list"));
        }
        let words = words(&prop.bytes);
        if words.len() / 2 > max_pairs {
            return Err(io::Error::new(io::ErrorKind::FileTooLarge, "atom pair list is too long"));
        }
        Ok(Some(
            words
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| (u64::from(pair[0]), u64::from(pair[1])))
                .collect(),
        ))
    }

    /// Read and delete a 32-bit property, e.g. TARGETS or TIMESTAMP.
    pub(crate) fn read_values32(&self, window: u64, property: u64, limit: usize) -> io::Result<Option<(u64, Vec<u32>)>> {
        let Some(prop) = self.query(window, property, limit.max(1)) else {
            return Ok(None);
        };
        if prop.format != 32 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "property is not 32-bit"));
        }
        if prop.bytes_after > 0 || prop.items > limit {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "property exceeds the limit"));
        }
        Ok(Some((prop.type_, words(&prop.bytes))))
    }

    fn query(&self, window: u64, property: u64, long_length: usize) -> Option<Property> {
        let mut type_ = 0u64;
        let mut format = 0i32;
        let mut items = 0u64;
        let mut bytes_after = 0u64;
        let mut data: *mut u8 = ptr::null_mut();
        // SAFETY: all outputs are initialized; data is XFree'd after copying.
        let status = unsafe {
            (self.xlib.XGetWindowProperty)(
                self.dpy,
                window,
                property,
                0,
                long_length as _,
                1,
                PROPERTY_TYPE_NONE,
                &mut type_,
                &mut format,
                &mut items,
                &mut bytes_after,
                &mut data,
            )
        };
        if status != 0 || type_ == 0 || format == 0 {
            if !data.is_null() {
                // SAFETY: data came from XGetWindowProperty.
                unsafe { (self.xlib.XFree)(data.cast()) };
            }
            return None;
        }
        let word = std::mem::size_of::<std::ffi::c_long>();
        let size = items as usize * if format == 32 { word } else { format as usize / 8 };
        let bytes = if data.is_null() {
            Vec::new()
        } else {
            // SAFETY: the library returned `size` initialized bytes.
            let bytes = unsafe { std::slice::from_raw_parts(data, size) }.to_vec();
            // SAFETY: data came from XGetWindowProperty.
            unsafe { (self.xlib.XFree)(data.cast()) };
            bytes
        };
        Some(Property {
            type_,
            format,
            items: items as usize,
            bytes_after: bytes_after as usize,
            bytes,
        })
    }

    pub(crate) fn selection_owner(&self, selection: u64) -> u64 {
        // SAFETY: plain query on a live connection.
        unsafe { (self.xlib.XGetSelectionOwner)(self.dpy, selection) }
    }

    pub(crate) fn display_name(&self) -> String {
        // SAFETY: Xlib owns the string for the display's lifetime; it is copied.
        unsafe {
            let name = (self.xlib.XDisplayString)(self.dpy);
            if name.is_null() {
                return String::new();
            }
            CStr::from_ptr(name).to_string_lossy().into_owned()
        }
    }

    pub(crate) fn set_selection_owner(&self, selection: u64, owner: u64, time: u64) {
        // SAFETY: owner is a window on this connection or None.
        unsafe { (self.xlib.XSetSelectionOwner)(self.dpy, selection, owner, time) };
    }

    pub(crate) fn convert(&self, selection: u64, target: u64, property: u64, requestor: u64, time: u64) {
        // SAFETY: requestor belongs to this connection; property may be absent.
        unsafe { (self.xlib.XConvertSelection)(self.dpy, selection, target, property, requestor, time) };
    }

    pub(crate) fn send_selection_notify(&self, requestor: u64, selection: u64, target: u64, property: u64) {
        // SAFETY: XEvent is plain old data; only the selection arm is initialized.
        let mut event: XEvent = unsafe { std::mem::zeroed() };
        {
            // SAFETY: this event carries the SelectionNotify arm written below.
            let event = unsafe { &mut event.selection };
            event.type_ = EVENT_TYPE_SELECTION_NOTIFY;
            event.requestor = requestor;
            event.selection = selection;
            event.target = target;
            event.property = property;
        }
        // SAFETY: marshalled event is sent to the requestor window.
        unsafe { (self.xlib.XSendEvent)(self.dpy, requestor, 0, 0, &mut event) };
    }

    pub(crate) fn pending(&self) -> bool {
        // SAFETY: flushes the output buffer as documented.
        unsafe { (self.xlib.XPending)(self.dpy) > 0 }
    }

    pub(crate) fn next_event(&self) -> XEvent {
        // SAFETY: only called when pending() is true.
        let mut event: XEvent = unsafe { std::mem::zeroed() };
        // SAFETY: only called when pending() is true and the union is writable.
        unsafe { (self.xlib.XNextEvent)(self.dpy, &mut event) };
        event
    }

    pub(crate) fn fd(&self) -> RawFd {
        // SAFETY: the display is open for the lifetime of self.
        unsafe { (self.xlib.XConnectionNumber)(self.dpy) }
    }

    pub(crate) fn sync(&self) {
        // SAFETY: discard=false; waits for the server to process pending requests.
        unsafe { (self.xlib.XSync)(self.dpy, 0) };
    }

    pub(crate) fn flush(&self) {
        // SAFETY: the display is open.
        unsafe { (self.xlib.XFlush)(self.dpy) };
    }

    /// Server time from a round trip; stable identity material for ownership.
    pub(crate) fn server_timestamp(
        &self,
        window: u64,
        property: u64,
        type_: u64,
        stash: &mut Vec<XEvent>,
    ) -> io::Result<u32> {
        self.change_property(window, property, type_, 8, &[]);
        self.flush();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            while self.pending() {
                let event = self.next_event();
                // SAFETY: the union is plain data; type_ is always initialized.
                let event_type = unsafe { event.type_ };
                if event_type == EVENT_TYPE_PROPERTY_NOTIFY {
                    // SAFETY: the event type selects the PropertyNotify arm.
                    let prop = unsafe { event.property };
                    if prop.window == window && prop.atom == property && prop.state == EVENT_STATE_NEW_VALUE {
                        self.delete_property(window, property);
                        return Ok(prop.time as u32);
                    }
                }
                stash.push(event);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "timestamp round trip"));
            }
            wait_readable(self.fd(), deadline - now)?;
        }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        // SAFETY: drops the only owner of this connection.
        unsafe { (self.xlib.XCloseDisplay)(self.dpy) };
    }
}
