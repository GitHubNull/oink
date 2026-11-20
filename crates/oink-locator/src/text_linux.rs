//! Linux 中文文字：Pango + Cairo（Fontconfig），系统自带的 Noto Sans CJK SC。
//!
//! 与 macOS/Windows 相同的 `TextRaster` 约定：按 cap（大写字母/数字墨迹高度）反推
//! 绝对字号，返回覆盖率图；`cap_top = pad + baseline − cap`、`left = pad`、
//! `advance = 逻辑宽度`。Pango 的全局对象不保证线程安全，所有布局与绘制经进程内
//! 互斥串行化，并按 (文本, cap, 字重) 做有界缓存。

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use cairo::{Context, Format, ImageSurface};
use glib::translate::{ToGlibPtr, from_glib_full};
use pango::{FontDescription, Layout, SCALE, Weight};

use super::{Mask, TextRaster};

/// 与 Windows 的微软雅黑、macOS 的系统字体同族角色：中文界面字体（Pango 自动回退拉丁与数字）。
const FAMILY: &str = "Noto Sans CJK SC";
/// `cairo-rs` 的 ARGB32 是原生端序；alpha 在内存中的字节位置随端序变化。
const ALPHA_BYTE: usize = if cfg!(target_endian = "little") { 3 } else { 0 };
/// 缓存上限：超过后整表清空，避免无界增长。
const CACHE_LIMIT: usize = 256;

static PANGO_LOCK: Mutex<()> = Mutex::new(());

#[link(name = "pangocairo-1.0")]
unsafe extern "C" {
    fn pango_cairo_create_layout(cr: *mut cairo::ffi::cairo_t) -> *mut pango::ffi::PangoLayout;
    fn pango_cairo_show_layout(cr: *mut cairo::ffi::cairo_t, layout: *mut pango::ffi::PangoLayout);
}

#[derive(Debug)]
pub(super) struct SystemText {
    /// 0：常规/加粗 400/700；1：中等 500；2：toast 标题 600 / 正文 450。
    mode: u8,
}

impl SystemText {
    pub(super) fn toast() -> Option<Self> {
        (cap_ratio(2, false).is_some() && cap_ratio(2, true).is_some()).then_some(Self { mode: 2 })
    }
    pub(super) fn medium() -> Option<Self> {
        cap_ratio(1, false).map(|_| Self { mode: 1 })
    }
    pub(super) fn new() -> Option<Self> {
        (cap_ratio(0, false).is_some() && cap_ratio(0, true).is_some()).then_some(Self { mode: 0 })
    }
}

impl TextRaster for SystemText {
    fn line(&self, text: &str, cap: f64, bold: bool) -> Mask {
        render(text, cap, bold, self.mode).unwrap_or_default()
    }
}

fn weight(mode: u8, bold: bool) -> i32 {
    match (mode, bold) {
        (2, true) => 600,
        (2, false) => 450,
        (1, _) => 500,
        (_, true) => 700,
        (_, false) => 400,
    }
}

/// 字形必须随线程可用；Pango 的初始化在互斥内完成。
fn layout_for(cr: &Context, text: &str, em: f64, weight: i32) -> Layout {
    // SAFETY: cr 是调用方持有的有效 Cairo 上下文；返回的布局由此函数接管所有权。
    let layout: Layout = unsafe { from_glib_full(pango_cairo_create_layout(cr.to_raw_none())) };
    layout.set_text(text);
    let mut font = FontDescription::new();
    font.set_family(FAMILY);
    font.set_weight(Weight::__Unknown(weight));
    // set_absolute_size 的单位是 PANGO_SCALE，不是像素。
    font.set_absolute_size(em * f64::from(SCALE));
    layout.set_font_description(Some(&font));
    layout
}

/// cap 高与字身的比例：量「H」的墨迹高度（按字重各算一次）。
fn cap_ratio(mode: u8, bold: bool) -> Option<f64> {
    static RATIO: [OnceLock<Option<f64>>; 6] = [const { OnceLock::new() }; 6];
    let _guard = PANGO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    *RATIO[usize::from(bold) + usize::from(mode) * 2].get_or_init(|| {
        let surface = ImageSurface::create(Format::ARgb32, 1, 1).ok()?;
        let context = Context::new(&surface).ok()?;
        let layout = layout_for(&context, "H", 200.0, weight(mode, bold));
        let (ink, _) = layout.pixel_extents();
        (ink.height() > 20).then(|| f64::from(ink.height()) / 200.0)
    })
}

type CacheKey = (String, u64, bool, u8);
type Cache = Mutex<HashMap<CacheKey, Mask>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn render(text: &str, cap: f64, bold: bool, mode: u8) -> Option<Mask> {
    let ratio = cap_ratio(mode, bold)?;
    let em = cap.max(1.0) / ratio;
    let key = (text.to_owned(), cap.to_bits(), bold, mode);
    if let Some(mask) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned() {
        return Some(mask);
    }
    let mask = {
        let _guard = PANGO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        rasterize(text, cap, em, weight(mode, bold))?
    };
    let mut map = cache().lock().unwrap_or_else(|e| e.into_inner());
    if map.len() > CACHE_LIMIT {
        map.clear();
    }
    map.insert(key, mask.clone());
    Some(mask)
}

fn rasterize(text: &str, cap: f64, em: f64, weight: i32) -> Option<Mask> {
    let scratch = ImageSurface::create(Format::ARgb32, 1, 1).ok()?;
    let scratch_context = Context::new(&scratch).ok()?;
    let measure = layout_for(&scratch_context, text, em, weight);
    let (ink, logical) = measure.pixel_extents();
    let baseline = f64::from(measure.baseline()) / f64::from(SCALE);
    drop(measure);
    drop(scratch_context);
    drop(scratch);
    let pad = (cap * 0.08).ceil().max(1.0);
    let advance = f64::from(logical.width());
    let ink_right = f64::from(ink.x()) + f64::from(ink.width());
    let w = (advance.max(ink_right) + pad * 2.0).ceil().max(1.0) as i32;
    let h = (f64::from(logical.height()) + pad * 2.0).ceil().max(1.0) as i32;
    let mut surface = ImageSurface::create(Format::ARgb32, w, h).ok()?;
    let context = Context::new(&surface).ok()?;
    context.set_source_rgba(0.0, 0.0, 0.0, 0.0);
    context.paint().ok()?;
    // Pango 没有前景属性时用当前 cairo 源色；透明源会把字形画成空。
    context.set_source_rgba(1.0, 1.0, 1.0, 1.0);
    let layout = layout_for(&context, text, em, weight);
    context.move_to(pad, pad);
    // SAFETY: cr 与 layout 分别是本函数持有的有效 Cairo/Pango 对象；show_layout 同步
    // 把字形画进 surface，不保留指针。
    unsafe { pango_cairo_show_layout(context.to_raw_none(), layout.to_glib_none().0) };
    drop(layout);
    drop(context);
    surface.flush();
    let stride = (surface.stride() as usize).max(w as usize * 4);
    let data = surface.data().ok()?;
    let alpha: Vec<u8> = (0..h as usize)
        .flat_map(|row| {
            let start = row * stride;
            data[start..start + w as usize * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[ALPHA_BYTE])
        })
        .collect();
    Some(Mask {
        w: w as usize,
        h: h as usize,
        alpha,
        cap_top: pad + baseline - cap,
        advance,
        left: pad,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(mask: &Mask) -> usize {
        mask.alpha.iter().filter(|alpha| **alpha > 0).count()
    }

    #[test]
    fn chinese_glyphs_have_ink_and_scale_with_cap() {
        let font = SystemText::new().unwrap();
        let small = font.line("命中率", 20.0, false);
        let big = font.line("命中率", 40.0, false);
        assert!(lit(&small) > 100, "Chinese glyphs are drawn: {}", lit(&small));
        assert!((big.w as f64 / small.w as f64 - 2.0).abs() < 0.15);
    }

    #[test]
    fn digit_ink_height_tracks_the_requested_cap() {
        let font = SystemText::new().unwrap();
        let mask = font.line("0", 40.0, true);
        let rows: Vec<usize> = (0..mask.h)
            .filter(|row| {
                mask.alpha[row * mask.w..(row + 1) * mask.w]
                    .iter()
                    .any(|alpha| *alpha > 128)
            })
            .collect();
        let (top, bottom) = (*rows.first().unwrap() as f64, *rows.last().unwrap() as f64 + 1.0);
        assert!(
            (bottom - top - 40.0).abs() <= 2.0,
            "digit height {} vs cap 40",
            bottom - top
        );
        assert!((top - mask.cap_top).abs() <= 3.0, "cap top {top} vs {}", mask.cap_top);
    }

    #[test]
    fn cache_keeps_weights_apart() {
        let font = SystemText::toast().unwrap();
        let regular = font.line("得分", 30.0, false);
        let bold = font.line("得分", 30.0, true);
        assert_ne!(regular, bold);
        assert!(lit(&bold) >= lit(&regular));
        // 同一键再次取用必须命中缓存并返回同样的覆盖率图。
        assert_eq!(regular, font.line("得分", 30.0, false));
        let empty = font.line("", 20.0, false);
        assert_eq!(empty.alpha.len(), empty.w * empty.h);
    }
}
