//! Nemo context-menu actions for Paste as File. Only a packaged layout
//! (the `oink` binary beside `installed.json`) registers actions, mirroring
//! the Windows rule; anything else removes our stale actions instead.
use super::{language, title};
use crate::config::Settings;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

const ACTION: &str = "oink-paste-as-file.nemo_action";
const BACKGROUND_ACTION: &str = "oink-paste-as-file-background.nemo_action";

pub(super) fn apply(exe: &Path, settings: &Settings) -> io::Result<()> {
    apply_in(exe, settings, actions_dir().ok().as_deref())
}

fn apply_in(exe: &Path, settings: &Settings, dir: Option<&Path>) -> io::Result<()> {
    if !packaged(exe) || !settings.paste_as_file {
        if let Some(dir) = dir {
            remove_actions_from(dir)?;
        } else {
            remove_actions()?;
        }
        return Ok(());
    }
    let Some(dir) = dir else {
        return Err(io::Error::new(io::ErrorKind::NotFound, "actions directory unavailable"));
    };
    let language = language(settings.ui_language);
    std::fs::create_dir_all(dir)?;
    write_action(&dir.join(ACTION), &action_contents(exe, language, false))?;
    write_action(&dir.join(BACKGROUND_ACTION), &action_contents(exe, language, true))?;
    Ok(())
}

/// The packaged app binary is always named `oink`; a build tree or the
/// service binary must never register desktop actions.
fn packaged(exe: &Path) -> bool {
    exe.file_name().is_some_and(|name| name == "oink") && exe.with_file_name("installed.json").is_file()
}

fn actions_dir() -> io::Result<PathBuf> {
    actions_dir_from(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_DATA_HOME and HOME unset"))
}

fn actions_dir_from(data_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = data_home
        .map(PathBuf::from)
        .or_else(|| home.map(|home| PathBuf::from(home).join(".local/share")))?;
    Some(base.join("nemo/actions"))
}

/// One action covers file and directory selections (`%f` is their path and
/// the backend resolves a selected file to its parent); the background
/// action receives the folder itself via `%P`.
fn action_contents(exe: &Path, language: &str, background: bool) -> String {
    let (selection, argument) = if background { ("None", "%P") } else { ("s", "%f") };
    format!(
        "[Nemo Action]\nName={name}\nExec=\"{exe}\" --paste-as-file {argument}\nSelection={selection}\nExtensions=any;\n",
        name = title(language),
        exe = exe.display(),
    )
}

fn write_action(path: &Path, contents: &str) -> io::Result<()> {
    oink_foundation::storage::atomic_write(path, contents.as_bytes())
}

fn remove_actions() -> io::Result<()> {
    let Ok(dir) = actions_dir() else {
        return Ok(());
    };
    remove_actions_from(&dir)
}

fn remove_actions_from(dir: &Path) -> io::Result<()> {
    for name in [ACTION, BACKGROUND_ACTION] {
        match std::fs::remove_file(dir.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(enabled: bool, language: crate::config::UiLanguage) -> Settings {
        Settings {
            paste_as_file: enabled,
            ui_language: language,
            ..Settings::default()
        }
    }

    #[test]
    fn action_contents_localize_and_differ_by_surface() {
        let exe = Path::new("/opt/oink/oink");
        let item = action_contents(exe, "zh-Hans", false);
        assert!(item.contains("Name=粘贴为文件"), "{item}");
        assert!(item.contains("Exec=\"/opt/oink/oink\" --paste-as-file %f"), "{item}");
        assert!(item.contains("Selection=s"), "{item}");
        assert!(item.contains("Extensions=any;"), "{item}");
        let background = action_contents(exe, "en", true);
        assert!(background.contains("Name=Paste as File"), "{background}");
        assert!(background.contains("--paste-as-file %P"), "{background}");
        assert!(background.contains("Selection=None"), "{background}");
    }

    #[test]
    fn actions_dir_prefers_xdg_then_home() {
        let from_xdg = actions_dir_from(Some("/xdg".into()), Some("/home".into())).unwrap();
        assert_eq!(from_xdg, PathBuf::from("/xdg/nemo/actions"));
        let from_home = actions_dir_from(None, Some("/home".into())).unwrap();
        assert_eq!(from_home, PathBuf::from("/home/.local/share/nemo/actions"));
        assert!(actions_dir_from(None, None).is_none());
    }

    #[test]
    fn packaged_layout_requires_sibling_installed_marker() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("oink");
        std::fs::write(&exe, b"").unwrap();
        assert!(!packaged(&exe));
        std::fs::write(root.path().join("installed.json"), b"{}").unwrap();
        assert!(packaged(&exe));
        assert!(!packaged(&root.path().join("oink-agent")));
    }

    #[test]
    fn apply_writes_and_removes_both_actions() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("oink");
        std::fs::write(&exe, b"").unwrap();
        std::fs::write(root.path().join("installed.json"), b"{}").unwrap();
        let data = tempfile::tempdir().unwrap();
        let actions = data.path().join("nemo/actions");
        let enabled = settings(true, crate::config::UiLanguage::Auto);
        apply_in(&exe, &enabled, Some(&actions)).unwrap();
        assert!(actions.join(ACTION).is_file());
        assert!(actions.join(BACKGROUND_ACTION).is_file());
        let disabled = settings(false, crate::config::UiLanguage::Auto);
        apply_in(&exe, &disabled, Some(&actions)).unwrap();
        assert!(!actions.join(ACTION).exists());
        assert!(!actions.join(BACKGROUND_ACTION).exists());
    }

    #[test]
    fn unpackaged_run_removes_stale_actions() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("oink-agent");
        std::fs::write(&exe, b"").unwrap();
        let data = tempfile::tempdir().unwrap();
        let actions = data.path().join("nemo/actions");
        std::fs::create_dir_all(&actions).unwrap();
        std::fs::write(actions.join(ACTION), b"stale").unwrap();
        apply_in(&exe, &settings(true, crate::config::UiLanguage::Auto), Some(&actions)).unwrap();
        assert!(!actions.join(ACTION).exists());
    }
}
