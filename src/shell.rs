//! Opening files and folders with the desktop's default applications.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, Result};

fn spawn_detached(mut command: Command) -> Result<()> {
    let program = format!("{:?}", command.get_program());
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| anyhow!("could not run {program}: {err}"))?;
    // Reap the process so it does not linger as a zombie on Unix.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Opens a file or folder with its default application.
pub fn open_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(anyhow!("{} no longer exists", path.display()));
    }
    #[cfg(windows)]
    let command = {
        let mut command = Command::new("explorer.exe");
        command.arg(path);
        command
    };
    #[cfg(target_os = "macos")]
    let command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };
    #[cfg(target_os = "linux")]
    let command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };
    spawn_detached(command)
}

/// Shows a file selected in the system file manager.
pub fn reveal_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(anyhow!("{} no longer exists", path.display()));
    }
    #[cfg(windows)]
    {
        let mut command = Command::new("explorer.exe");
        command.arg(format!("/select,{}", path.display()));
        spawn_detached(command)
    }
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg("-R").arg(path);
        spawn_detached(command)
    }
    #[cfg(target_os = "linux")]
    {
        // The FileManager1 D-Bus interface selects the file in Nautilus,
        // Dolphin, Nemo, Thunar, ... Fall back to opening the parent folder.
        let uri = format!("file://{}", path.display());
        let shown = Command::new("dbus-send")
            .args([
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
                &format!("array:string:{uri}"),
                "string:",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if shown {
            return Ok(());
        }
        open_path(path.parent().unwrap_or(path))
    }
}

/// Shows the native folder picker. Blocks until the user decides.
pub fn pick_folder(start: &Path, title: &str) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    if start.is_dir() {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder()
}

/// Whether the folder picker must run on the UI thread (macOS AppKit rule).
pub const PICKER_ON_MAIN_THREAD: bool = cfg!(target_os = "macos");
