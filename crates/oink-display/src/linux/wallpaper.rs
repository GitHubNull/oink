//! Cinnamon 壁纸：读 gsettings 的图片、显示方式与底色，按屏幕绘制缩略图；变化由专用 GLib
//! 上下文观察。只读取设置，不写；壁纸可能是私人照片，不记录路径、内容或由它们算出的值。
//!
//! 拿不到可解码的静态图片（`none`、空 URI、远端 URI、文件不存在、解码失败）时返回空列表，
//! 不当作显示失败；没有 Cinnamon schema 时返回 `None`（本平台暂时无法提供缩略图）。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use gdk_pixbuf::{Colorspace, InterpType, Pixbuf};
use gio::prelude::*;
use oink_foundation::display::DisplayInfo;

use crate::wallpaper::{DisplayWallpaper, TARGET_BYTES, Thumbnail, cover_rect, thumbnail_size};
use crate::{ChangeHandler, DisplayError};

const SCHEMA_ID: &str = "org.cinnamon.desktop.background";
/// 依次尝试的 JPEG 质量：第一个不超过 [`TARGET_BYTES`] 的结果就用；都超过时用最后一个。
const QUALITIES: [u32; 4] = [72, 55, 40, 28];
const INTERP: InterpType = InterpType::Bilinear;

pub(super) fn read_all(displays: &[DisplayInfo]) -> Option<Vec<DisplayWallpaper>> {
    let (uri, options, background) = current_settings()?;
    let path = match image_path(&uri) {
        Some(path) => path,
        None => return Some(Vec::new()),
    };
    let image = match Pixbuf::from_file(&path) {
        Ok(image) if image.width() > 0 && image.height() > 0 => image,
        _ => return Some(Vec::new()),
    };
    let fill = parse_color(&background);
    let union = union_rect(displays);
    let mut out = Vec::new();
    for display in displays {
        if let Some(thumbnail) = render(&image, &options, display, union, fill) {
            out.push(DisplayWallpaper {
                key: display.key.clone(),
                thumbnail,
            });
        }
    }
    Some(out)
}

fn current_settings() -> Option<(String, String, String)> {
    let schema = gio::SettingsSchemaSource::default()?.lookup(SCHEMA_ID, true)?;
    let settings = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
    Some((
        settings.string("picture-uri").to_string(),
        settings.string("picture-options").to_string(),
        settings.string("primary-color").to_string(),
    ))
}

fn image_path(uri: &str) -> Option<PathBuf> {
    if uri.is_empty() {
        return None;
    }
    let (path, host) = glib::filename_from_uri(uri).ok()?;
    // 只接受本机文件；远端主机与非 file URI 都按「没有静态图片」处理。
    if host.is_some_and(|host| !host.is_empty() && host != "localhost") {
        return None;
    }
    path.is_file().then_some(path)
}

fn parse_color(text: &str) -> [u8; 3] {
    let hex = text.strip_prefix('#').unwrap_or(text);
    match hex.len() {
        6 => u32::from_str_radix(hex, 16)
            .ok()
            .map(|v| [(v >> 16) as u8, (v >> 8) as u8, v as u8]),
        3 => u32::from_str_radix(hex, 16).ok().map(|v| {
            let [r, g, b] = [((v >> 8) & 0xF) as u8, ((v >> 4) & 0xF) as u8, (v & 0xF) as u8];
            [r * 17, g * 17, b * 17]
        }),
        _ => None,
    }
    .unwrap_or([0; 3])
}

fn union_rect(displays: &[DisplayInfo]) -> Option<(f64, f64, f64, f64)> {
    let mut combined: Option<(f64, f64, f64, f64)> = None;
    for display in displays {
        let (x, y, w, h) = (display.bounds.x, display.bounds.y, display.bounds.w, display.bounds.h);
        if ![x, y, w, h].into_iter().all(f64::is_finite) || w <= 0.0 || h <= 0.0 {
            continue;
        }
        combined = Some(match combined {
            Some((cx, cy, cw, ch)) => {
                let left = cx.min(x);
                let top = cy.min(y);
                let right = (cx + cw).max(x + w);
                let bottom = (cy + ch).max(y + h);
                (left, top, right - left, bottom - top)
            }
            None => (x, y, w, h),
        });
    }
    combined
}

fn render(
    image: &Pixbuf,
    option: &str,
    display: &DisplayInfo,
    union: Option<(f64, f64, f64, f64)>,
    fill: [u8; 3],
) -> Option<Thumbnail> {
    let (w, h) = (display.bounds.w, display.bounds.h);
    let (tw, th) = thumbnail_size(w, h)?;
    let canvas = Pixbuf::new(Colorspace::Rgb, false, 8, tw as i32, th as i32)?;
    canvas.fill((u32::from(fill[0]) << 24) | (u32::from(fill[1]) << 16) | (u32::from(fill[2]) << 8) | 0xFF);
    let scale = f64::from(tw) / w;
    let (iw, ih) = (f64::from(image.width()), f64::from(image.height()));
    let (tf_w, tf_h) = (f64::from(tw), f64::from(th));
    match option {
        "zoom" => {
            let (x, y, cw, _) = cover_rect(iw, ih, w, h);
            let factor = cw / iw;
            draw(image, &canvas, x * scale, y * scale, factor * scale, factor * scale);
        }
        "stretched" => {
            draw(image, &canvas, 0.0, 0.0, tf_w / iw, tf_h / ih);
        }
        "scaled" => {
            let factor = (w / iw).min(h / ih);
            draw(
                image,
                &canvas,
                (w - iw * factor) / 2.0 * scale,
                (h - ih * factor) / 2.0 * scale,
                factor * scale,
                factor * scale,
            );
        }
        "centered" => {
            draw(
                image,
                &canvas,
                (tf_w - iw * scale) / 2.0,
                (tf_h - ih * scale) / 2.0,
                scale,
                scale,
            );
        }
        "wallpaper" => tile(image, &canvas, scale),
        "spanned" => spanned(image, &canvas, display, union?, scale),
        _ => return None,
    }
    let jpeg = encode(&canvas)?;
    Some(Thumbnail {
        width: tw as u16,
        height: th as u16,
        jpeg,
    })
}

fn draw(image: &Pixbuf, canvas: &Pixbuf, x: f64, y: f64, scale_x: f64, scale_y: f64) {
    image.composite(
        canvas,
        0,
        0,
        canvas.width(),
        canvas.height(),
        x,
        y,
        scale_x,
        scale_y,
        INTERP,
        255,
    );
}

/// 平铺：在原图像素尺寸（乘缩略图比例）的格子上重复；格子小于 2 像素时用平均色代替，
/// 避免极小图片产生几十万次合成，缩略图观感不变。
fn tile(image: &Pixbuf, canvas: &Pixbuf, scale: f64) {
    let tile_w = f64::from(image.width()) * scale;
    let tile_h = f64::from(image.height()) * scale;
    if tile_w < 2.0 || tile_h < 2.0 {
        canvas.fill(average_pixel(image));
        return;
    }
    let (tw, th) = (f64::from(canvas.width()), f64::from(canvas.height()));
    let mut y = 0.0;
    while y < th {
        let mut x = 0.0;
        while x < tw {
            draw(image, canvas, x, y, scale, scale);
            x += tile_w;
        }
        y += tile_h;
    }
}

fn average_pixel(image: &Pixbuf) -> u32 {
    let bytes = image.read_pixel_bytes();
    let data: &[u8] = &bytes;
    let channels = image.n_channels().max(1) as usize;
    let stride = image.rowstride() as usize;
    let (w, h) = (image.width() as usize, image.height() as usize);
    let (mut r, mut g, mut b, mut count) = (0u64, 0u64, 0u64, 0u64);
    let step_x = (w / 32).max(1);
    let step_y = (h / 32).max(1);
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let index = y * stride + x * channels;
            if index + 2 < data.len() {
                r += u64::from(data[index]);
                g += u64::from(data[index + 1]);
                b += u64::from(data[index + 2]);
                count += 1;
            }
            x += step_x;
        }
        y += step_y;
    }
    if count == 0 {
        return 0xFF;
    }
    let [r, g, b] = [r / count, g / count, b / count];
    ((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | 0xFF
}

/// 跨屏：图片铺满所有活动屏幕的联合区域，本屏取与图片重叠的部分。
fn spanned(image: &Pixbuf, canvas: &Pixbuf, display: &DisplayInfo, union: (f64, f64, f64, f64), scale: f64) {
    let (ux, uy, uw, uh) = union;
    let (iw, ih) = (f64::from(image.width()), f64::from(image.height()));
    let (w, h) = (display.bounds.w, display.bounds.h);
    let (sx, sy) = (display.bounds.x, display.bounds.y);
    let factor = (uw / iw).max(uh / ih);
    let (ix, iy) = (ux + (uw - iw * factor) / 2.0, uy + (uh - ih * factor) / 2.0);
    let cx = sx.max(ix);
    let cy = sy.max(iy);
    let cw = (sx + w).min(ix + iw * factor) - cx;
    let ch = (sy + h).min(iy + ih * factor) - cy;
    if cw <= 0.0 || ch <= 0.0 {
        return;
    }
    let src_x = ((cx - ix) / factor).floor().max(0.0) as i32;
    let src_y = ((cy - iy) / factor).floor().max(0.0) as i32;
    let src_w = ((cw / factor).ceil() as i32).max(1).min(image.width() - src_x);
    let src_h = ((ch / factor).ceil() as i32).max(1).min(image.height() - src_y);
    if src_w <= 0 || src_h <= 0 {
        return;
    }
    let part = image.new_subpixbuf(src_x, src_y, src_w, src_h);
    draw(
        &part,
        canvas,
        (cx - sx) * scale,
        (cy - sy) * scale,
        factor * scale,
        factor * scale,
    );
}

fn encode(canvas: &Pixbuf) -> Option<Vec<u8>> {
    let mut last = None;
    for quality in QUALITIES {
        let quality = quality.to_string();
        let jpeg = canvas.save_to_bufferv("jpeg", &[("quality", quality.as_str())]).ok()?;
        let done = jpeg.len() <= TARGET_BYTES;
        last = Some(jpeg);
        if done {
            break;
        }
    }
    last
}

/// 壁纸变化监听线程的句柄；丢弃即停止并等待线程退出。
pub(super) struct Watch {
    stop: Arc<AtomicBool>,
    context: glib::MainContext,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // 唤醒可能正阻塞的 iteration。上下文无人持有时会直接执行这个空操作，同样安全；
        // 线程接下来一定会在循环条件处看到 stop。
        self.context.invoke(|| {});
        if let Some(handle) = self.handle.take()
            && handle.thread().id() != std::thread::current().id()
        {
            let _ = handle.join();
        }
    }
}

pub(super) fn watch(on_change: ChangeHandler) -> Result<Watch, DisplayError> {
    let context = glib::MainContext::new();
    let stop = Arc::new(AtomicBool::new(false));
    let (ready, ready_rx) = std::sync::mpsc::channel();
    let (thread_context, thread_stop) = (context.clone(), stop.clone());
    let handle = std::thread::Builder::new()
        .name("oink-wallpaper-watch".into())
        .spawn(move || watch_loop(thread_context, thread_stop, on_change, ready))
        .map_err(|error| DisplayError::Platform(format!("壁纸变化线程启动失败：{error}")))?;
    // 等到 connect_changed 全部装好再返回，否则调用方紧接着的第一次变更会丢失。
    // 线程提前退出时发送端随之丢弃；有界等待避免异常线程卡住调用方。
    let _ = ready_rx.recv_timeout(std::time::Duration::from_secs(2));
    Ok(Watch {
        stop,
        context,
        handle: Some(handle),
    })
}

fn watch_loop(
    context: glib::MainContext,
    stop: Arc<AtomicBool>,
    on_change: ChangeHandler,
    ready: std::sync::mpsc::Sender<()>,
) {
    let _ = context.with_thread_default(|| {
        let Some(schema) = gio::SettingsSchemaSource::default().and_then(|source| source.lookup(SCHEMA_ID, true))
        else {
            tracing::debug!("壁纸变化监听未启动：没有 {SCHEMA_ID}");
            return;
        };
        let settings = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
        let notify = {
            let on_change = on_change.clone();
            move |_: &gio::Settings, _: &str| on_change()
        };
        let _handlers = [
            settings.connect_changed(Some("picture-uri"), notify.clone()),
            settings.connect_changed(Some("picture-options"), notify.clone()),
            settings.connect_changed(Some("primary-color"), notify),
        ];
        let _ = ready.send(());
        // 本线程持有上下文：其他线程的 invoke 只会排队并唤醒 iteration，
        // 不会在循环开始前直接执行，因此不存在 quit-before-run 竞态。
        while !stop.load(Ordering::Acquire) {
            context.iteration(true);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use oink_foundation::{Rect, Size};

    fn display(key: &str, x: f64, y: f64, w: f64, h: f64) -> DisplayInfo {
        DisplayInfo {
            key: key.into(),
            name: key.into(),
            bounds: Rect::new(x, y, w, h),
            pixel_width: w as u32,
            pixel_height: h as u32,
            reference_extent: None::<Size>,
            rotation_deg: 0,
            is_primary: false,
            is_builtin: false,
            is_virtual: false,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oink-wallpaper-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn accepts_local_files_and_rejects_remote_or_missing_images() {
        let dir = temp_dir("uri");
        let path = dir.join("image one.png");
        std::fs::write(&path, b"fixture").unwrap();
        let uri = glib::filename_to_uri(&path, None).unwrap().to_string();
        assert!(uri.contains("%20"));
        assert_eq!(image_path(&uri), Some(path));
        assert_eq!(image_path(""), None);
        assert_eq!(image_path("file://remotehost/tmp/image.png"), None);
        assert_eq!(image_path("https://example.com/image.png"), None);
        assert_eq!(image_path(&format!("file://{}/missing.png", dir.display())), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parses_hex_colors_and_falls_back_to_black() {
        assert_eq!(parse_color("#ffffff"), [255, 255, 255]);
        assert_eq!(parse_color("#000000"), [0, 0, 0]);
        assert_eq!(parse_color("#abc"), [170, 187, 204]);
        assert_eq!(parse_color("rgb(1,2,3)"), [0, 0, 0]);
        assert_eq!(parse_color(""), [0, 0, 0]);
    }

    #[test]
    fn union_covers_negative_origins_and_skips_invalid_bounds() {
        let displays = [
            display("a", 0.0, 0.0, 100.0, 100.0),
            display("b", -50.0, 20.0, 50.0, 40.0),
            display("bad", 999.0, 999.0, 0.0, 100.0),
        ];
        assert_eq!(union_rect(&displays), Some((-50.0, 0.0, 150.0, 100.0)));
    }

    #[test]
    fn renders_each_cinnamon_option_to_a_bounded_jpeg() {
        let image = Pixbuf::new(Colorspace::Rgb, false, 8, 320, 200).unwrap();
        image.fill(0x336699FF);
        let screen = display("screen", 0.0, 0.0, 1600.0, 1000.0);
        let union = Some((0.0, 0.0, 1600.0, 1000.0));
        for option in ["zoom", "stretched", "scaled", "centered", "wallpaper", "spanned"] {
            let thumbnail =
                render(&image, option, &screen, union, [0, 0, 0]).unwrap_or_else(|| panic!("{option} thumbnail"));
            assert_eq!((thumbnail.width, thumbnail.height), (480, 300));
            assert!(thumbnail.jpeg.starts_with(&[0xFF, 0xD8]), "{option}");
            assert!(thumbnail.jpeg.len() <= TARGET_BYTES, "{option}");
        }
        assert!(render(&image, "none", &screen, union, [0, 0, 0]).is_none());
        assert!(render(&image, "spanned", &screen, None, [0, 0, 0]).is_none());
    }
}
