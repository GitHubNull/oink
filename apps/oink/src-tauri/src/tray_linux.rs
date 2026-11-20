//! Linux X11 tray through libappindicator/StatusNotifierItem.
//!
//! The tray is optional: hosts without a StatusNotifierWatcher keep the full
//! WebView UI and only lose the tray entry. Menu events use the same backend
//! model as the Windows tray; the panel entry is local and always available.
use gio::glib::ToVariant;
use oink_agent::backend::{PanelAnchor, PanelRect};
use oink_agent::config::UiLanguage;
use tauri::{AppHandle, PhysicalPosition, PhysicalRect};

const TRAY_ID: &str = "main";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

pub(crate) fn install(app: &AppHandle) {
    if !watcher_available() {
        tracing::warn!("StatusNotifierWatcher absent; Linux tray disabled");
        return;
    }
    let icon = match tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png")) {
        Ok(icon) => icon,
        Err(error) => {
            tracing::warn!(%error, "Linux tray icon could not be decoded");
            return;
        }
    };
    if let Err(error) = tauri::tray::TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("Oink")
        .show_menu_on_left_click(true)
        .build(app)
    {
        tracing::warn!(%error, "Linux tray could not be created");
    }
}

pub(crate) fn update(app: &AppHandle, model: &oink_agent::native_controls::Model, language: UiLanguage) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let label = oink_agent::native_controls::panel_label(language);
    match crate::native_controls::build_menu(app, model, Some(label)) {
        Ok(menu) => {
            if let Err(error) = tray.set_menu(Some(menu)) {
                tracing::warn!(%error, "Linux tray menu update failed");
            }
        }
        Err(error) => tracing::warn!(%error, "Linux tray menu could not be built"),
    }
}

/// A hidden tray is not an error when the host has no watcher; the setting still
/// applies to this process without a user-facing failure.
pub(crate) fn set_visible(app: &AppHandle, visible: bool) -> Result<(), ()> {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return Ok(());
    };
    tray.set_visible(visible).map_err(|error| {
        tracing::warn!(%error, "Linux tray visibility update failed");
    })
}

pub(crate) fn toggle_panel(app: &AppHandle) {
    match panel_anchor(app) {
        Some(anchor) => crate::shell::toggle_panel(app, anchor),
        None => tracing::warn!("could not resolve the Linux panel anchor"),
    }
}

/// AppIndicator exposes no tray rectangle. The clicked cursor position anchors
/// the panel instead; the 1x1 rectangle is clamped by the monitor work area.
fn panel_anchor(app: &AppHandle) -> Option<PanelAnchor> {
    let cursor = app.cursor_position().ok()?;
    let monitor = app
        .monitor_from_point(cursor.x, cursor.y)
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten())?;
    Some(anchor_from(cursor, monitor.work_area(), monitor.scale_factor()))
}

fn anchor_from(cursor: PhysicalPosition<f64>, work: &PhysicalRect<i32, u32>, scale: f64) -> PanelAnchor {
    PanelAnchor {
        rect: PanelRect {
            x: cursor.x,
            y: cursor.y,
            width: 1.0,
            height: 1.0,
        },
        work_area: PanelRect {
            x: f64::from(work.position.x),
            y: f64::from(work.position.y),
            width: f64::from(work.size.width),
            height: f64::from(work.size.height),
        },
        scale_factor: scale,
    }
}

/// D-Bus replies carry the out-arguments as a tuple; a bare `bool` read always
/// fails, which silently reported every existing watcher as absent.
fn name_owned(reply: &gio::glib::Variant) -> bool {
    reply.get::<(bool,)>().is_some_and(|(owned,)| owned)
}

fn watcher_available() -> bool {
    let query = || -> Result<bool, gio::glib::Error> {
        let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
        let reply = bus.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(WATCHER,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            1_000,
            gio::Cancellable::NONE,
        )?;
        Ok(name_owned(&reply))
    };
    match query() {
        Ok(available) => available,
        Err(error) => {
            tracing::warn!(%error, "StatusNotifierWatcher could not be queried; Linux tray disabled");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::{PhysicalRect, PhysicalSize};

    #[test]
    fn name_owner_reply_reads_the_tuple_body() {
        assert!(name_owned(&(true,).to_variant()));
        assert!(!name_owned(&(false,).to_variant()));
        assert!(!name_owned(&"true".to_variant()));
    }

    #[test]
    fn cursor_anchor_keeps_physical_work_area_and_scale() {
        let work = PhysicalRect {
            position: PhysicalPosition::new(-1920, 24),
            size: PhysicalSize::new(1920, 1056),
        };
        let anchor = anchor_from(PhysicalPosition::new(-1900.0, 40.0), &work, 1.5);
        assert_eq!(
            anchor.rect,
            PanelRect {
                x: -1900.0,
                y: 40.0,
                width: 1.0,
                height: 1.0
            }
        );
        assert_eq!(
            anchor.work_area,
            PanelRect {
                x: -1920.0,
                y: 24.0,
                width: 1920.0,
                height: 1056.0
            }
        );
        assert_eq!(anchor.scale_factor, 1.5);
    }
}
