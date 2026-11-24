use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let primary = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let primary = std::env::var_os("HOME");
    primary
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Reads an XDG user directory (`XDG_VIDEOS_DIR`, ...) from
/// `~/.config/user-dirs.dirs`.
#[cfg(target_os = "linux")]
fn xdg_user_dir(key: &str) -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"));
    let text = std::fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
    let home = home_dir();
    text.lines().find_map(|line| {
        let value = line
            .trim()
            .strip_prefix(key)?
            .trim_start()
            .strip_prefix('=')?;
        let value = value.trim().trim_matches('"');
        let path = match value.strip_prefix("$HOME") {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(value),
        };
        (path != home).then_some(path)
    })
}

pub fn recordings_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    let base = home_dir().join("Movies");
    #[cfg(target_os = "linux")]
    let base = xdg_user_dir("XDG_VIDEOS_DIR").unwrap_or_else(|| home_dir().join("Videos"));
    #[cfg(windows)]
    let base = home_dir().join("Videos");
    base.join("Scroder")
}

pub fn screenshots_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    let base = xdg_user_dir("XDG_PICTURES_DIR").unwrap_or_else(|| home_dir().join("Pictures"));
    #[cfg(not(target_os = "linux"))]
    let base = home_dir().join("Pictures");
    base.join("Scroder")
}

pub fn config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(|p| PathBuf::from(p).join("Scroder"))
            .or_else(|| Some(home_dir().join("AppData").join("Roaming").join("Scroder")))
    }
    #[cfg(target_os = "macos")]
    {
        Some(
            home_dir()
                .join("Library")
                .join("Application Support")
                .join("Scroder"),
        )
    }
    #[cfg(target_os = "linux")]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home_dir().join(".config"));
        Some(base.join("scroder"))
    }
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn timestamped_path(dir: &Path, ext: &str) -> PathBuf {
    let (y, m, d, hh, mm, ss, ms) = local_parts();
    dir.join(format!(
        "Scroder_{y:04}-{m:02}-{d:02}_{hh:02}-{mm:02}-{ss:02}_{ms:03}.{ext}"
    ))
}

#[cfg(windows)]
fn local_parts() -> (u32, u32, u32, u32, u32, u32, u32) {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let now = unsafe { GetLocalTime() };
    (
        now.wYear as u32,
        now.wMonth as u32,
        now.wDay as u32,
        now.wHour as u32,
        now.wMinute as u32,
        now.wSecond as u32,
        now.wMilliseconds as u32,
    )
}

#[cfg(unix)]
fn local_parts() -> (u32, u32, u32, u32, u32, u32, u32) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&secs, &mut tm);
    }
    (
        (tm.tm_year + 1900) as u32,
        (tm.tm_mon + 1) as u32,
        tm.tm_mday as u32,
        tm.tm_hour as u32,
        tm.tm_min as u32,
        tm.tm_sec as u32,
        now.subsec_millis(),
    )
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn time_ago(timestamp: i64) -> String {
    if timestamp <= 0 {
        return String::new();
    }
    let delta = (unix_now() - timestamp).max(0);
    match delta {
        0..=44 => "just now".to_string(),
        45..=3_599 => format!("{} min ago", (delta / 60).max(1)),
        3_600..=86_399 => format!("{} h ago", delta / 3_600),
        86_400..=172_799 => "yesterday".to_string(),
        _ => format!("{} days ago", delta / 86_400),
    }
}

pub fn human_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// "12.4 Mbps"
pub fn human_bitrate(bits_per_second: u64) -> String {
    let mbps = bits_per_second as f64 / 1_000_000.0;
    if mbps >= 10.0 {
        format!("{mbps:.0} Mbps")
    } else {
        format!("{mbps:.1} Mbps")
    }
}
