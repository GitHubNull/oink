//! Phase 5 XDND probe: drives the product DnD backend against native GTK peers
//! inside the disposable Xephyr session. Emits one JSON object per line; drag
//! payloads are reported as lengths and SHA-256 digests only, never as text.
//!
//!   phase5_dnd_probe catcher --x N --y N     # product catcher under a GTK source
//!   phase5_dnd_probe source --x N --y N [--size N] [--gen N]  # product source
//!
//! Both modes then read one JSON command per line on stdin:
//!   catcher: {"cmd":"hold"} {"cmd":"resume","x":N,"y":N}
//!            {"cmd":"finish","accept":bool} {"cmd":"unprobe"} {"cmd":"quit"}
//!   source:  {"cmd":"move","x":N,"y":N} {"cmd":"release"} {"cmd":"cancel"}
//!            {"cmd":"quit"}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("phase5_dnd_probe is Linux/X11 only");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
fn main() {
    imp::run();
}

#[cfg(target_os = "linux")]
mod imp {
    use oink_clipboard::ClipData;
    use oink_clipboard::dnd::{self, DndBackend, DragOperation, DragOps, DragSpec, PlatformDndEvent};
    use oink_foundation::Point;
    use oink_foundation::input::{InjectEvent, MouseButton};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::io::{self, BufRead, Write};
    use std::sync::{Arc, Mutex};

    const TEXT: &str = "oink-phase5-dnd-text";
    /// Test-bridge atom the GTK target role fences payload reads with; a real
    /// peer never looks at it. Only the probe (the drag source peer) sets it.
    const GENERATION_ATOM: &str = "_OINK_TEST_DND_GENERATION";

    pub(crate) fn run() {
        let mut args = std::env::args().skip(1);
        let mode = args.next().unwrap_or_default();
        let mut opts = HashMap::new();
        while let Some(arg) = args.next() {
            let Some(name) = arg.strip_prefix("--") else {
                emit(json!({"event": "error", "reason": "unexpected argument", "arg": arg}));
                std::process::exit(2);
            };
            let value = match name {
                "x" | "y" | "size" | "gen" => {
                    let Some(value) = args.next() else {
                        emit(json!({"event": "error", "reason": "missing option value", "option": name}));
                        std::process::exit(2);
                    };
                    value
                }
                _ => {
                    emit(json!({"event": "error", "reason": "unknown option", "option": name}));
                    std::process::exit(2);
                }
            };
            opts.insert(name.to_owned(), value);
        }
        let result = match mode.as_str() {
            "catcher" => catcher(&opts),
            "source" => source(&opts),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown mode {other}"),
            )),
        };
        if let Err(error) = result {
            emit(json!({
                "event": "error",
                "kind": format!("{:?}", error.kind()),
                "reason": error.to_string(),
            }));
            std::process::exit(1);
        }
    }

    fn number(opts: &HashMap<String, String>, name: &str) -> io::Result<f64> {
        opts.get(name)
            .map(|text| text.parse().map_err(|_| bad_input(name)))
            .transpose()
            .map(|value| value.unwrap_or(0.0))
    }

    fn bad_input(name: &str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidInput, name.to_string())
    }

    fn emit(value: Value) {
        let stdout = io::stdout();
        let mut guard = stdout.lock();
        let _ = writeln!(guard, "{value}");
        let _ = guard.flush();
    }

    fn digest(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn pattern(len: usize) -> String {
        (0..len).map(|i| char::from(b'a' + (i % 26) as u8)).collect()
    }

    fn item_summary(items: &[ClipData]) -> Vec<Value> {
        items
            .iter()
            .map(|item| match item {
                ClipData::Text(text) => {
                    json!({"kind": "text", "length": text.len(), "sha256": digest(text.as_bytes())})
                }
                ClipData::Html(html) => {
                    json!({"kind": "html", "length": html.len(), "sha256": digest(html.as_bytes())})
                }
                ClipData::Rtf(rtf) => json!({"kind": "rtf", "length": rtf.len(), "sha256": digest(rtf)}),
                ClipData::Png(png) => json!({"kind": "png", "length": png.len(), "sha256": digest(png)}),
                ClipData::Files(entries) => json!({
                    "kind": "files",
                    "count": entries.len(),
                    "names_sha256": digest(
                        entries.iter().map(|entry| entry.path.as_str()).collect::<String>().as_bytes()),
                }),
                ClipData::FileRoots(nodes) => json!({"kind": "file-roots", "count": nodes.len()}),
                ClipData::Custom { name, bytes, .. } => {
                    json!({"kind": "custom", "name": name, "length": bytes.len(), "sha256": digest(bytes)})
                }
                ClipData::Native { .. } => json!({"kind": "native"}),
            })
            .collect()
    }

    /// Shared sink: report every backend event as JSON and remember the last
    /// detected operation so stdin commands can address the live session.
    fn sink(state: &Arc<Mutex<ProbeState>>) -> oink_clipboard::dnd::DndSink {
        let state = state.clone();
        Arc::new(move |event| match event {
            PlatformDndEvent::Detected { operation, items, ops } => {
                state.lock().unwrap_or_else(|e| e.into_inner()).operation = Some(operation);
                emit(json!({
                    "event": "detected",
                    "operation": operation.sequence,
                    "ops": ops.0,
                    "items": item_summary(&items),
                }));
            }
            PlatformDndEvent::SourceEnded { operation, accepted } => {
                emit(json!({
                    "event": "source-ended",
                    "operation": operation.sequence,
                    "accepted": accepted,
                }));
            }
            PlatformDndEvent::SourceCatcherLost { operation } => {
                emit(json!({"event": "catcher-lost", "operation": operation.sequence}));
            }
            PlatformDndEvent::Ended { operation, dropped } => {
                state.lock().unwrap_or_else(|e| e.into_inner()).ended = true;
                emit(json!({
                    "event": "ended",
                    "operation": operation.sequence,
                    "dropped": dropped,
                }));
            }
            _ => {}
        })
    }

    struct ProbeState {
        operation: Option<DragOperation>,
        ended: bool,
    }

    fn start_backend(state: &Arc<Mutex<ProbeState>>) -> io::Result<Box<dyn DndBackend>> {
        let backend = dnd::platform_dnd().map_err(|error| io::Error::other(format!("{error:?}")))?;
        let xtest = Arc::new(XTest::open()?);
        let injector: oink_clipboard::dnd::Injector = {
            let xtest = xtest.clone();
            Arc::new(move |event| xtest.inject(event))
        };
        backend
            .start(sink(state), injector)
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        Ok(backend)
    }

    fn commands() -> impl Iterator<Item = io::Result<Value>> {
        io::stdin().lock().lines().map(|line| {
            let line = line?;
            serde_json::from_str(&line).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        })
    }

    fn point(opts: &HashMap<String, String>) -> io::Result<Point> {
        Ok(Point::new(number(opts, "x")?, number(opts, "y")?))
    }

    fn catcher(opts: &HashMap<String, String>) -> io::Result<()> {
        let state = Arc::new(Mutex::new(ProbeState {
            operation: None,
            ended: false,
        }));
        let backend = start_backend(&state)?;
        let at = point(opts)?;
        backend.probe(at);
        emit(json!({"event": "ready", "mode": "catcher", "x": at.x, "y": at.y}));
        for command in commands() {
            let command = command?;
            let name = command.get("cmd").and_then(Value::as_str).unwrap_or_default();
            let operation = state.lock().unwrap_or_else(|e| e.into_inner()).operation;
            let Some(operation) = operation else {
                if name == "quit" {
                    return Ok(());
                }
                emit(json!({"event": "error", "reason": "no drag detected yet", "cmd": name}));
                continue;
            };
            match name {
                "hold" => match backend.hold_source(operation) {
                    Ok(anchor) => emit(json!({
                        "event": "held",
                        "operation": operation.sequence,
                        "x": anchor.at.x,
                        "y": anchor.at.y,
                        "surface": anchor.surface,
                        "generation": anchor.generation,
                    })),
                    Err(error) => emit(json!({"event": "error", "cmd": "hold", "reason": format!("{error:?}")})),
                },
                "resume" => {
                    let at = Some(Point::new(
                        command.get("x").and_then(Value::as_f64).unwrap_or_default(),
                        command.get("y").and_then(Value::as_f64).unwrap_or_default(),
                    ));
                    let ok = backend.resume_source_at(operation, at);
                    emit(json!({"event": "resumed", "operation": operation.sequence, "ok": ok}));
                }
                "finish" => {
                    let accept = command.get("accept").and_then(Value::as_bool).unwrap_or(false);
                    let xtest = XTest::open()?;
                    let ok = backend.finish_source(operation, accept, &|| {
                        xtest.inject(&InjectEvent::Button {
                            button: MouseButton::Left,
                            pressed: false,
                        })
                    });
                    emit(json!({
                        "event": "finish-ack",
                        "operation": operation.sequence,
                        "accept": accept,
                        "ok": ok,
                    }));
                }
                "unprobe" => backend.unprobe(),
                "quit" => return Ok(()),
                other => emit(json!({"event": "error", "reason": "unknown command", "cmd": other})),
            }
        }
        Ok(())
    }

    fn source(opts: &HashMap<String, String>) -> io::Result<()> {
        let state = Arc::new(Mutex::new(ProbeState {
            operation: None,
            ended: false,
        }));
        let backend = start_backend(&state)?;
        let size = opts
            .get("size")
            .map(|text| text.parse::<usize>())
            .transpose()
            .map_err(|_| bad_input("size"))?;
        let text = size.map(pattern).unwrap_or_else(|| TEXT.to_owned());
        let length = text.len();
        let sha256 = digest(text.as_bytes());
        let spec = DragSpec {
            operation: DragOperation::new(),
            at: point(opts)?,
            items: vec![ClipData::Text(text)],
            files: None,
            native: None,
            ops: DragOps::COPY,
        };
        let operation = spec.operation;
        backend
            .begin(spec)
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        let xtest = XTest::open()?;
        let generation = opts
            .get("gen")
            .map(|text| text.parse::<u64>())
            .transpose()
            .map_err(|_| bad_input("gen"))?;
        let owner = xtest.mark_generation(generation)?;
        emit(json!({
            "event": "begin-ack",
            "operation": operation.sequence,
            "selection_owner": owner,
            "generation": generation.unwrap_or(0),
            "length": length,
            "sha256": sha256,
        }));
        // Source mode is one-shot: stdin is drained on a thread so the loop
        // can notice an asynchronous drag end without a further command.
        let (command_tx, command_rx) = std::sync::mpsc::channel::<io::Result<Value>>();
        std::thread::spawn(move || {
            for line in commands() {
                if command_tx.send(line).is_err() {
                    break;
                }
            }
        });
        loop {
            let command = match command_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(command) => command?,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if state.lock().unwrap_or_else(|e| e.into_inner()).ended {
                        return Ok(());
                    }
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            };
            let name = command.get("cmd").and_then(Value::as_str).unwrap_or_default();
            match name {
                "move" => {
                    let at = Point::new(
                        command.get("x").and_then(Value::as_f64).unwrap_or_default(),
                        command.get("y").and_then(Value::as_f64).unwrap_or_default(),
                    );
                    if let Err(error) = xtest.inject(&InjectEvent::MoveTo { pos: at }) {
                        emit(json!({"event": "error", "cmd": "move", "reason": error}));
                    }
                }
                "release" => {
                    if let Err(error) = xtest.inject(&InjectEvent::Button {
                        button: MouseButton::Left,
                        pressed: false,
                    }) {
                        emit(json!({"event": "error", "cmd": "release", "reason": error}));
                    }
                }
                "cancel" => {
                    let ok = backend.cancel();
                    emit(json!({"event": "cancel-ack", "operation": operation.sequence, "ok": ok}));
                }
                "quit" => return Ok(()),
                other => emit(json!({"event": "error", "reason": "unknown command", "cmd": other})),
            }
            if state.lock().unwrap_or_else(|e| e.into_inner()).ended {
                return Ok(());
            }
        }
    }

    /// XTEST injection on a dedicated connection; also sets the test-bridge
    /// generation property on the XdndSelection owner for the GTK target role.
    struct XTest {
        xlib: x11_dl::xlib::Xlib,
        xtest: x11_dl::xtest::Xf86vmode,
        display: *mut x11_dl::xlib::Display,
    }

    // SAFETY: the raw display pointer is only touched under the Xlib locks
    // held by XInitThreads, and each method borrows &self for the call.
    unsafe impl Send for XTest {}
    // SAFETY: as for Send; every use serializes on the same locks.
    unsafe impl Sync for XTest {}

    impl XTest {
        fn open() -> io::Result<Self> {
            let xlib = x11_dl::xlib::Xlib::open().map_err(io::Error::other)?;
            // SAFETY: XInitThreads ran in the backend process init; the display
            // pointer is only used from inject/mark_generation with Xlib locks.
            let display = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
            if display.is_null() {
                return Err(io::Error::new(io::ErrorKind::NotConnected, "XOpenDisplay failed"));
            }
            let xtest = x11_dl::xtest::Xf86vmode::open().map_err(io::Error::other)?;
            Ok(Self { xlib, xtest, display })
        }

        fn inject(&self, event: &InjectEvent) -> Result<(), String> {
            let ok = match event {
                InjectEvent::MoveTo { pos } => {
                    // SAFETY: display is open; XTEST reports success nonzero.
                    unsafe {
                        (self.xtest.XTestFakeMotionEvent)(
                            self.display,
                            -1,
                            pos.x.round() as i32,
                            pos.y.round() as i32,
                            0,
                        )
                    }
                }
                InjectEvent::Button { button, pressed } => {
                    let number = match button {
                        MouseButton::Left => 1,
                        MouseButton::Right => 3,
                        MouseButton::Middle => 2,
                        MouseButton::Back => 8,
                        MouseButton::Forward => 9,
                        _ => return Err("unsupported button".to_owned()),
                    };
                    // SAFETY: display is open; XTEST reports success nonzero.
                    unsafe { (self.xtest.XTestFakeButtonEvent)(self.display, number, i32::from(*pressed), 0) }
                }
                _ => return Err("unsupported inject event".to_owned()),
            };
            // SAFETY: display is open.
            unsafe { (self.xlib.XFlush)(self.display) };
            if ok == 0 {
                return Err("XTEST injection rejected".to_owned());
            }
            Ok(())
        }

        fn mark_generation(&self, generation: Option<u64>) -> io::Result<u64> {
            let selection = self.intern("XdndSelection")?;
            // SAFETY: the display is open; the call only reads server state.
            let owner = unsafe { (self.xlib.XGetSelectionOwner)(self.display, selection) };
            if owner == 0 {
                return Err(io::Error::new(io::ErrorKind::NotFound, "XdndSelection has no owner"));
            }
            if let Some(generation) = generation {
                let atom = self.intern(GENERATION_ATOM)?;
                let value = generation as std::ffi::c_ulong;
                // SAFETY: owner is a live window id returned by the server.
                unsafe {
                    (self.xlib.XChangeProperty)(
                        self.display,
                        owner,
                        atom,
                        6, // XA_CARDINAL
                        32,
                        0, // PropModeReplace
                        &value as *const std::ffi::c_ulong as *const u8,
                        1,
                    );
                    (self.xlib.XSync)(self.display, 0);
                }
            }
            Ok(owner)
        }

        fn intern(&self, name: &str) -> io::Result<u64> {
            let name = std::ffi::CString::new(name).map_err(|_| bad_input("atom name"))?;
            // SAFETY: name is NUL-terminated; the atom is server-owned.
            let atom = unsafe { (self.xlib.XInternAtom)(self.display, name.as_ptr(), 0) };
            if atom == 0 {
                return Err(io::Error::new(io::ErrorKind::NotFound, "atom missing"));
            }
            Ok(atom)
        }
    }

    impl Drop for XTest {
        fn drop(&mut self) {
            // SAFETY: the display was opened by this struct and no thread uses
            // it after the last inject/mark_generation call returned.
            unsafe { (self.xlib.XCloseDisplay)(self.display) };
        }
    }
}
