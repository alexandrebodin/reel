#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

use anyhow::{anyhow, Result};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

const VIDEO_KINDS: [&str; 5] = ["mp4", "mov", "webm", "mkv", "json"];
const IMAGE_KINDS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "heic"];
const OPEN_ID: &str = "open";
static NEXT_WINDOW: AtomicU32 = AtomicU32::new(1);

fn main() {
    add_tool_folders_to_path();
    let opened: Option<PathBuf> = std::env::args().nth(1).map(PathBuf::from);
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .menu(build_menu)
        .on_menu_event(|app, event| {
            if event.id() == OPEN_ID {
                open_in_new_window(app.clone(), None);
            }
        })
        .setup(move |app| {
            open_in_new_window(app.handle().clone(), opened.clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("running the app");
}

/// An app started from the Finder gets a bare PATH, which leaves out where ffmpeg is installed.
fn add_tool_folders_to_path() {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut folders: Vec<PathBuf> = std::env::split_paths(&current).collect();
    folders.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    if let Ok(path) = std::env::join_paths(folders) {
        std::env::set_var("PATH", path);
    }
}

/// Leaves undo and redo out of the Edit menu: a menu shortcut would take ⌘Z before the page sees it.
fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let open = MenuItem::with_id(app, OPEN_ID, "Open…", true, Some("CmdOrCtrl+O"))?;
    let name = app.package_info().name.clone();
    Menu::with_items(
        app,
        &[
            &Submenu::with_items(app, &name, true, &[&PredefinedMenuItem::about(app, None, None)?, &PredefinedMenuItem::separator(app)?, &PredefinedMenuItem::hide(app, None)?, &PredefinedMenuItem::quit(app, None)?])?,
            &Submenu::with_items(app, "File", true, &[&open, &PredefinedMenuItem::close_window(app, None)?])?,
            &Submenu::with_items(app, "Edit", true, &[&PredefinedMenuItem::cut(app, None)?, &PredefinedMenuItem::copy(app, None)?, &PredefinedMenuItem::paste(app, None)?, &PredefinedMenuItem::select_all(app, None)?])?,
            &Submenu::with_items(app, "Window", true, &[&PredefinedMenuItem::minimize(app, None)?, &PredefinedMenuItem::fullscreen(app, None)?])?,
        ],
    )
}

fn open_in_new_window(app: AppHandle, path: Option<PathBuf>) {
    thread::spawn(move || {
        let path = path.or_else(|| {
            let picked = app.dialog().file().set_title("Open a video or a reel recording").add_filter("Video or recording", &VIDEO_KINDS).blocking_pick_file();
            picked.and_then(|file| file.into_path().ok())
        });
        let Some(path) = path else {
            if app.webview_windows().is_empty() {
                app.exit(0);
            }
            return;
        };
        if let Err(error) = open_project(&app, &path) {
            app.dialog().message(format!("{error:#}")).title(format!("Could not open {}", path.display())).kind(MessageDialogKind::Error).blocking_show();
            if app.webview_windows().is_empty() {
                app.exit(1);
            }
        }
    });
}

fn open_project(app: &AppHandle, path: &Path) -> Result<()> {
    let label = format!("project-{}", NEXT_WINDOW.fetch_add(1, Ordering::Relaxed));
    let title = path.file_name().map_or("Reel".to_string(), |name| name.to_string_lossy().into_owned());
    let window = WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html".into())).title(title).inner_size(1440.0, 900.0).min_inner_size(960.0, 640.0).build()?;
    let opened = (|| {
        let picker_app = app.clone();
        let editor = reel::open_editor(path)?.with_image_picker(Box::new(move || {
            let picked = picker_app.dialog().file().set_title("Choose a background image").add_filter("Image", &IMAGE_KINDS).blocking_pick_file();
            picked.and_then(|file| file.into_path().ok())
        }));
        let url = reel::serve_editor(editor)?;
        window.navigate(url.parse().map_err(|e| anyhow!("{e}"))?)?;
        Ok(())
    })();
    if opened.is_err() {
        let _ = window.destroy();
    }
    opened
}
