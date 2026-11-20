//! Linux X11：按 XRandR 枚举显示器、监听配置变化，提供原生 origin 事务与 Cinnamon 壁纸缩略图。
//!
//! 只支持 X11；Wayland 会话由后台入口在创建任何后端之前拒绝，这里不处理。
//! 本机坐标为 X11 根窗口像素（scale 恒为 1）。排列写入只在显式事务中发生；
//! 普通枚举与布局同步不改系统布局。

mod arrangement;
mod enumerate;
mod wallpaper;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use oink_foundation::display::DisplayInfo;
use x11_dl::xlib::{self, Display, Xlib};
use x11_dl::xrandr::{self as xrr, Xrandr};

use super::arrangement::NativeConfiguration;
use super::wallpaper::DisplayWallpaper;
use super::{ChangeHandler, DisplayBackend, DisplayError};

pub(crate) struct X11Display {
    xlib: Xlib,
    xrandr: Xrandr,
    watchers: Arc<Mutex<Vec<ChangeHandler>>>,
    watch: Mutex<Option<JoinHandle<()>>>,
    stop: Arc<AtomicBool>,
    wallpaper_watchers: Arc<Mutex<Vec<ChangeHandler>>>,
    wallpaper_watch: Mutex<Option<wallpaper::Watch>>,
}

impl std::fmt::Debug for X11Display {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Display").finish_non_exhaustive()
    }
}

impl Drop for X11Display {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.watch.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }
        drop(self.wallpaper_watch.lock().unwrap_or_else(|e| e.into_inner()).take());
    }
}

pub(crate) fn backend() -> Result<Box<dyn DisplayBackend>, DisplayError> {
    let xlib = Xlib::open().map_err(|e| DisplayError::Platform(format!("加载 libX11 失败：{e}")))?;
    let xrandr = Xrandr::open().map_err(|e| DisplayError::Platform(format!("加载 libXrandr 失败：{e}")))?;
    // 枚举连接与变化监听线程各持独立连接；全局初始化必须先于其他 X 调用。
    // SAFETY: 无参数、进程内幂等，此时尚未发出任何 X 请求。
    unsafe { (xlib.XInitThreads)() };
    let backend = X11Display {
        xlib,
        xrandr,
        watchers: Arc::default(),
        watch: Mutex::new(None),
        stop: Arc::new(AtomicBool::new(false)),
        wallpaper_watchers: Arc::default(),
        wallpaper_watch: Mutex::new(None),
    };
    // 创建即验证：把连接失败暴露在启动阶段，而不是第一次枚举时。
    backend.displays()?;
    Ok(Box::new(backend))
}

impl DisplayBackend for X11Display {
    fn native_configuration(&self) -> Result<NativeConfiguration, DisplayError> {
        arrangement::configuration(&self.xlib, &self.xrandr)
    }

    fn apply_native_origins(
        &self,
        expected: &NativeConfiguration,
        origins: &[super::arrangement::NativeOrigin],
    ) -> Result<NativeConfiguration, DisplayError> {
        arrangement::apply(&self.xlib, &self.xrandr, expected, origins)
    }

    fn displays(&self) -> Result<Vec<DisplayInfo>, DisplayError> {
        let dpy = open_display(&self.xlib)?;
        let result = enumerate::enumerate(&self.xlib, &self.xrandr, dpy);
        // SAFETY: dpy came from XOpenDisplay in this function and is closed exactly once here.
        unsafe {
            (self.xlib.XCloseDisplay)(dpy);
        }
        result
    }

    fn watch(&self, on_change: ChangeHandler) -> Result<(), DisplayError> {
        self.watchers.lock().unwrap_or_else(|e| e.into_inner()).push(on_change);
        let mut watch = self.watch.lock().unwrap_or_else(|e| e.into_inner());
        if watch.is_some() {
            return Ok(());
        }
        let xlib = Xlib::open().map_err(|e| DisplayError::Platform(format!("加载 libX11 失败：{e}")))?;
        let xrandr = Xrandr::open().map_err(|e| DisplayError::Platform(format!("加载 libXrandr 失败：{e}")))?;
        let watchers = self.watchers.clone();
        let stop = self.stop.clone();
        let handle = std::thread::Builder::new()
            .name("oink-display-watch".into())
            .spawn(move || watch_loop(&xlib, &xrandr, &stop, &watchers))
            .map_err(|e| DisplayError::Platform(format!("显示器变化线程启动失败：{e}")))?;
        *watch = Some(handle);
        Ok(())
    }

    fn wallpapers(&self) -> Option<Vec<DisplayWallpaper>> {
        let displays = self.displays().ok()?;
        wallpaper::read_all(&displays)
    }

    fn watch_wallpaper(&self, on_change: ChangeHandler) -> Result<(), DisplayError> {
        self.wallpaper_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(on_change);
        let mut watch = self.wallpaper_watch.lock().unwrap_or_else(|e| e.into_inner());
        if watch.is_some() {
            return Ok(());
        }
        let watchers = self.wallpaper_watchers.clone();
        let notify: ChangeHandler = Arc::new(move || {
            let handlers = watchers.lock().unwrap_or_else(|e| e.into_inner()).clone();
            for handler in handlers {
                handler();
            }
        });
        *watch = Some(wallpaper::watch(notify)?);
        Ok(())
    }
}

fn open_display(xlib: &Xlib) -> Result<*mut Display, DisplayError> {
    // SAFETY: null selects the DISPLAY environment variable, as every X client does.
    let dpy = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
    if dpy.is_null() {
        return Err(DisplayError::Platform(format!(
            "无法连接 X11 显示服务器（DISPLAY={:?}）",
            std::env::var("DISPLAY").ok()
        )));
    }
    Ok(dpy)
}

fn watch_loop(xlib: &Xlib, xrandr: &Xrandr, stop: &AtomicBool, watchers: &Mutex<Vec<ChangeHandler>>) {
    let dpy = match open_display(xlib) {
        Ok(dpy) => dpy,
        Err(error) => {
            tracing::warn!(%error, "显示器变化监听未启动");
            return;
        }
    };
    let mut event_base = 0;
    let mut error_base = 0;
    // SAFETY: dpy is an open connection and both out-pointers are valid storage.
    if unsafe { (xrandr.XRRQueryExtension)(dpy, &mut event_base, &mut error_base) } == 0 {
        tracing::warn!("显示器变化监听未启动：X 服务器没有 XRandR 扩展");
        // SAFETY: dpy came from XOpenDisplay above and is closed exactly once.
        unsafe {
            (xlib.XCloseDisplay)(dpy);
        }
        return;
    }
    // SAFETY: root belongs to this connection; selecting input is a no-op on an empty mask.
    unsafe {
        let root = (xlib.XDefaultRootWindow)(dpy);
        (xrandr.XRRSelectInput)(
            dpy,
            root,
            xrr::RRScreenChangeNotifyMask | xrr::RRCrtcChangeNotifyMask | xrr::RROutputChangeNotifyMask,
        );
        (xlib.XFlush)(dpy);
    }
    // SAFETY: XEvent is a plain 24-long union; zeroed storage is a valid event buffer.
    let mut event: xlib::XEvent = unsafe { std::mem::zeroed() };
    while !stop.load(Ordering::Acquire) {
        let mut changed = false;
        // SAFETY: XNextEvent fills the buffer for events XPending reported on this connection;
        // XRRUpdateConfiguration only reads the same buffer.
        unsafe {
            while (xlib.XPending)(dpy) > 0 {
                (xlib.XNextEvent)(dpy, &mut event);
                let kind = event.type_;
                if kind == event_base + xrr::RRScreenChangeNotify {
                    (xrandr.XRRUpdateConfiguration)(&mut event);
                    changed = true;
                } else if kind == event_base + xrr::RRNotify {
                    changed = true;
                }
            }
        }
        if changed {
            // Copy out so a callback may register another watcher without deadlocking.
            let handlers = watchers.lock().unwrap_or_else(|e| e.into_inner()).clone();
            for handler in handlers {
                handler();
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // SAFETY: this thread owns dpy and closes it once before returning.
    unsafe {
        (xlib.XCloseDisplay)(dpy);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arrangement::{ArrangementError, NativeOrigin};
    use gio::prelude::*;
    use oink_foundation::Point;
    use std::sync::atomic::AtomicUsize;

    #[test]
    #[ignore = "需要私有 Xephyr：DISPLAY=:0 python3 build-support/linux-native-test.py --timeout 240 -- cargo test -p oink-display linux::tests::xephyr_ -- --ignored --test-threads=1"]
    fn xephyr_native_configuration_is_stable_and_stale_writes_are_refused() {
        let backend = backend().expect("X11 后端");
        let config = backend.native_configuration().expect("原生快照");
        assert_eq!(config.displays.len(), 1, "{:?}", config);
        let display = &config.displays[0];
        assert!(display.primary, "{:?}", display);
        assert!(display.key.starts_with("x11:"), "{}", display.key);
        assert!(display.bounds.w > 0.0 && display.bounds.h > 0.0);
        let origins = config
            .sources()
            .map(|source| NativeOrigin {
                key: source.key.clone(),
                origin: Point::new(source.bounds.x, source.bounds.y),
            })
            .collect::<Vec<_>>();
        // 单屏主屏固定在原点：no-op 不写，返回同一份观测。
        let observed = backend.apply_native_origins(&config, &origins).expect("no-op 事务");
        assert_eq!(observed, config);
        // 陈旧基线被拒绝，桌面保持原样。
        let mut stale = config.clone();
        stale.displays[0].bounds.w += 1.0;
        let error = backend
            .apply_native_origins(&stale, &origins)
            .expect_err("陈旧基线必须被拒绝");
        assert!(
            matches!(error, DisplayError::Arrangement(ArrangementError::Changed)),
            "{error}"
        );
        assert_eq!(backend.native_configuration().unwrap(), config);
    }

    fn cinnamon_settings() -> gio::Settings {
        let schema = gio::SettingsSchemaSource::default()
            .and_then(|source| source.lookup("org.cinnamon.desktop.background", true))
            .expect("Cinnamon 壁纸 schema");
        gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None)
    }

    #[test]
    #[ignore = "需要私有 Xephyr 与 Cinnamon schema：DISPLAY=:0 python3 build-support/linux-native-test.py --timeout 240 -- cargo test -p oink-display linux::tests::xephyr_ -- --ignored --test-threads=1"]
    fn xephyr_wallpaper_reads_watches_and_handles_none_and_missing() {
        let root = std::env::temp_dir().join(format!("oink-wallpaper-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // SAFETY: 这个探针按 --test-threads=1 单独运行，且这是本进程第一次创建 GIO
        // 设置对象；keyfile 后端只写临时目录，不碰真实 dconf。
        unsafe {
            std::env::set_var("GSETTINGS_BACKEND", "keyfile");
            std::env::set_var("XDG_CONFIG_HOME", &root);
        }
        let source = root.join("source.png");
        let image = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 640, 480).unwrap();
        image.fill(0x2266AAFF);
        image.savev(&source, "png", &[]).unwrap();

        let settings = cinnamon_settings();
        settings
            .set_string("picture-uri", &glib::filename_to_uri(&source, None).unwrap())
            .unwrap();
        settings.set_string("picture-options", "zoom").unwrap();
        settings.set_string("primary-color", "#000000").unwrap();
        gio::Settings::sync();

        let backend = backend().expect("X11 后端");
        let displays = backend.displays().unwrap();
        let wallpapers = backend.wallpapers().expect("壁纸缩略图");
        assert_eq!(wallpapers.len(), displays.len());
        assert_eq!(wallpapers[0].key, displays[0].key);
        let thumbnail = &wallpapers[0].thumbnail;
        assert_eq!((thumbnail.width, thumbnail.height), (480, 300));
        assert!(thumbnail.jpeg.starts_with(&[0xFF, 0xD8]));
        assert!(thumbnail.jpeg.len() <= crate::wallpaper::TARGET_BYTES);

        let fired = Arc::new(AtomicUsize::new(0));
        let counter = fired.clone();
        backend
            .watch_wallpaper(Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }))
            .unwrap();
        settings.set_string("picture-options", "scaled").unwrap();
        gio::Settings::sync();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while fired.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(fired.load(Ordering::SeqCst) > 0, "没有收到壁纸变化");

        settings.set_string("picture-options", "none").unwrap();
        gio::Settings::sync();
        assert_eq!(backend.wallpapers(), Some(Vec::new()));

        let missing = glib::filename_to_uri(root.join("missing.png"), None)
            .unwrap()
            .to_string();
        settings.set_string("picture-uri", &missing).unwrap();
        gio::Settings::sync();
        assert_eq!(backend.wallpapers(), Some(Vec::new()));

        settings.reset("picture-options");
        settings.reset("picture-uri");
        settings.reset("primary-color");
        gio::Settings::sync();
        drop(settings);
        drop(backend);
        let _ = std::fs::remove_dir_all(&root);
    }
}
