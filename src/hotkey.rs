//! System-wide F9 / F10 / F11 shortcuts.
//!
//! Each listener wakes the UI when a key is pressed, so shortcuts work while
//! ScreenRec is minimized or in the background.

use std::sync::mpsc::{self, Receiver, Sender};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyAction {
    ToggleRecord,
    TogglePause,
    Screenshot,
}

pub struct HotkeyListener {
    rx: Receiver<HotkeyAction>,
    _inner: imp::Inner,
}

impl HotkeyListener {
    pub fn new(ctx: egui::Context) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let inner = imp::Inner::start(Notifier { tx, ctx })?;
        Ok(Self { rx, _inner: inner })
    }

    pub fn poll(&self) -> Vec<HotkeyAction> {
        self.rx.try_iter().collect()
    }
}

#[derive(Clone)]
struct Notifier {
    tx: Sender<HotkeyAction>,
    ctx: egui::Context,
}

impl Notifier {
    fn send(&self, action: HotkeyAction) {
        let _ = self.tx.send(action);
        self.ctx.request_repaint();
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use super::{HotkeyAction, Notifier};

    extern "system" {
        fn GetAsyncKeyState(v_key: i32) -> i16;
    }

    const KEYS: [(i32, HotkeyAction); 3] = [
        (0x78, HotkeyAction::ToggleRecord), // VK_F9
        (0x79, HotkeyAction::TogglePause),  // VK_F10
        (0x7A, HotkeyAction::Screenshot),   // VK_F11
    ];

    pub struct Inner {
        stop: Arc<AtomicBool>,
        join: Option<JoinHandle<()>>,
    }

    impl Inner {
        pub fn start(notifier: Notifier) -> Result<Self, String> {
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = stop.clone();
            let join = thread::Builder::new()
                .name("screenrec-hotkeys".to_string())
                .spawn(move || {
                    let mut was_down = [false; KEYS.len()];
                    while !thread_stop.load(Ordering::Relaxed) {
                        for (i, (key, action)) in KEYS.iter().enumerate() {
                            let state = unsafe { GetAsyncKeyState(*key) };
                            let down = (state as u16 & 0x8000) != 0;
                            if down && !was_down[i] {
                                notifier.send(*action);
                            }
                            was_down[i] = down;
                        }
                        thread::sleep(Duration::from_millis(30));
                    }
                })
                .map_err(|err| err.to_string())?;
            Ok(Self {
                stop,
                join: Some(join),
            })
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod imp {
    use global_hotkey::hotkey::{Code, HotKey};
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

    use super::{HotkeyAction, Notifier};

    pub struct Inner {
        manager: GlobalHotKeyManager,
        keys: Vec<HotKey>,
    }

    impl Inner {
        pub fn start(notifier: Notifier) -> Result<Self, String> {
            let manager = GlobalHotKeyManager::new().map_err(|err| err.to_string())?;
            let bindings = [
                (HotKey::new(None, Code::F9), HotkeyAction::ToggleRecord),
                (HotKey::new(None, Code::F10), HotkeyAction::TogglePause),
                (HotKey::new(None, Code::F11), HotkeyAction::Screenshot),
            ];

            let mut keys = Vec::new();
            let mut failed = Vec::new();
            for (key, _) in &bindings {
                match manager.register(*key) {
                    Ok(()) => keys.push(*key),
                    Err(_) => failed.push(format!("{:?}", key.key)),
                }
            }
            if keys.is_empty() {
                return Err("another application is already using F9, F10 and F11".into());
            }

            let map: Vec<(u32, HotkeyAction)> = bindings
                .iter()
                .map(|(key, action)| (key.id(), *action))
                .collect();
            GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
                if event.state != HotKeyState::Pressed {
                    return;
                }
                if let Some((_, action)) = map.iter().find(|(id, _)| *id == event.id) {
                    notifier.send(*action);
                }
            }));

            if !failed.is_empty() {
                eprintln!(
                    "warning: could not register global shortcut(s): {}",
                    failed.join(", ")
                );
            }
            Ok(Self { manager, keys })
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            GlobalHotKeyEvent::set_event_handler(None::<fn(GlobalHotKeyEvent)>);
            let _ = self.manager.unregister_all(&self.keys);
        }
    }
}
