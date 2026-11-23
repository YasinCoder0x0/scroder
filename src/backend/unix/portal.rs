//! xdg-desktop-portal integration (Linux): ScreenCast sessions for Wayland
//! recording and Screenshot.
//!
//! A ScreenCast session lives on its own worker thread. Every recording
//! segment asks the worker for a fresh PipeWire remote and hands it to a
//! `gst-launch-1.0 pipewiresrc` process, whose y4m output is piped into
//! ffmpeg.

use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::screenshot::Screenshot;
use ashpd::desktop::PersistMode;
use ashpd::enumflags2::BitFlags;

use super::{ffmpeg, linux, PreparedVideo, SessionHandle, VideoInput};
use crate::media::MissingDependency;

enum PortalCommand {
    OpenRemote(Sender<Result<OwnedFd, String>>),
    Close,
}

/// A running ScreenCast session.
pub struct Portal {
    commands: Sender<PortalCommand>,
    worker: Option<JoinHandle<()>>,
}

impl Portal {
    fn open_remote(&self) -> Result<OwnedFd> {
        let (tx, rx) = mpsc::channel();
        self.commands
            .send(PortalCommand::OpenRemote(tx))
            .map_err(|_| anyhow!("the screen sharing session has ended"))?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| anyhow!("the screen sharing portal did not respond"))?
            .map_err(|err| anyhow!("could not connect to PipeWire: {err}"))
    }
}

impl Drop for Portal {
    fn drop(&mut self) {
        let _ = self.commands.send(PortalCommand::Close);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Started {
    node_id: u32,
    height: u32,
}

fn token_path(window: bool) -> Option<PathBuf> {
    crate::util::config_dir().map(|dir| {
        dir.join(if window {
            "portal-window.token"
        } else {
            "portal-screen.token"
        })
    })
}

/// Opens the system screen-sharing dialog (or silently restores the previous
/// choice) and prepares a PipeWire video input.
pub fn prepare(window: bool, cursor: bool) -> Result<PreparedVideo> {
    if let Some(missing) = missing_dependencies().into_iter().next() {
        bail!("{} {}", missing.title, missing.detail);
    }

    let token = token_path(window)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());

    let (ready_tx, ready_rx) = mpsc::channel::<Result<Started, String>>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<PortalCommand>();

    let worker = std::thread::Builder::new()
        .name("screenrec-portal".into())
        .spawn(move || {
            pollster::block_on(async move {
                let setup = async {
                    let proxy = Screencast::new().await?;
                    let session = proxy.create_session(Default::default()).await?;

                    let sources: BitFlags<SourceType> = if window {
                        SourceType::Window.into()
                    } else {
                        SourceType::Monitor.into()
                    };
                    let mut options = SelectSourcesOptions::default()
                        .set_sources(Some(sources))
                        .set_multiple(false)
                        .set_persist_mode(PersistMode::ExplicitlyRevoked)
                        .set_restore_token(token.as_deref());
                    if let Ok(modes) = proxy.available_cursor_modes().await {
                        let wanted = if cursor {
                            CursorMode::Embedded
                        } else {
                            CursorMode::Hidden
                        };
                        if modes.contains(wanted) {
                            options = options.set_cursor_mode(wanted);
                        }
                    }
                    proxy.select_sources(&session, options).await?;
                    let streams = proxy
                        .start(&session, None, Default::default())
                        .await?
                        .response()?;
                    ashpd::Result::Ok((proxy, session, streams))
                };

                let (proxy, session, streams) = match setup.await {
                    Ok(value) => value,
                    Err(err) => {
                        let message = match &err {
                            ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {
                                "Screen sharing was cancelled.".to_string()
                            }
                            _ => format!("The screen sharing portal failed: {err}"),
                        };
                        let _ = ready_tx.send(Err(message));
                        return;
                    }
                };

                if let (Some(token), Some(path)) = (streams.restore_token(), token_path(window)) {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    let _ = std::fs::write(path, token);
                }

                let Some(stream) = streams.streams().first() else {
                    let _ = ready_tx.send(Err("No screen or window was shared.".into()));
                    let _ = session.close().await;
                    return;
                };
                let height = stream.size().map(|(_, h)| h.max(0) as u32).unwrap_or(1080);
                let _ = ready_tx.send(Ok(Started {
                    node_id: stream.pipe_wire_node_id(),
                    height,
                }));

                while let Ok(command) = cmd_rx.recv() {
                    match command {
                        PortalCommand::OpenRemote(reply) => {
                            let result = proxy
                                .open_pipe_wire_remote(&session, Default::default())
                                .await
                                .map_err(|err| err.to_string());
                            let _ = reply.send(result);
                        }
                        PortalCommand::Close => break,
                    }
                }
                let _ = session.close().await;
            });
        })
        .context("failed to start the portal worker")?;

    let portal = Portal {
        commands: cmd_tx,
        worker: Some(worker),
    };

    // The user may take a while to pick a source in the dialog.
    let started = ready_rx
        .recv_timeout(Duration::from_secs(300))
        .map_err(|_| anyhow!("Timed out waiting for the screen sharing dialog."))?
        .map_err(|err| anyhow!(err))?;

    Ok(PreparedVideo {
        input: VideoInput::PipeWire {
            node_id: started.node_id,
        },
        crop: None,
        out_height: started.height,
        session: Some(SessionHandle::Portal(portal)),
        warnings: Vec::new(),
    })
}

/// Starts `gst-launch-1.0` streaming the PipeWire node as y4m on stdout.
pub fn spawn_producer(portal: &Portal, node_id: u32, fps: u32) -> Result<Child> {
    let gst = ffmpeg::find_tool("gst-launch-1.0")
        .ok_or_else(|| anyhow!("gst-launch-1.0 was not found"))?;
    let remote = portal.open_remote()?;
    let raw = remote.as_raw_fd();

    let mut command = Command::new(gst);
    command
        .args([
            "-q",
            "-e",
            "pipewiresrc",
            "fd=3",
            &format!("path={node_id}"),
            "do-timestamp=true",
            "keepalive-time=1000",
            "!",
            "videorate",
            "!",
            &format!("video/x-raw,framerate={fps}/1"),
            "!",
            "videoconvert",
            "!",
            "video/x-raw,format=I420",
            "!",
            "y4menc",
            "!",
            "fdsink",
            "fd=1",
            "sync=false",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(move || {
            // Hand the PipeWire remote to the child as fd 3.
            if raw == 3 {
                if libc::fcntl(3, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            } else if libc::dup2(raw, 3) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    super::process_setup(&mut command);

    let child = command
        .spawn()
        .context("failed to start the PipeWire capture (gst-launch-1.0)")?;
    drop(remote);
    Ok(child)
}

fn gst_has_element(name: &str) -> bool {
    match ffmpeg::find_tool("gst-inspect-1.0") {
        Some(inspect) => Command::new(inspect)
            .arg(name)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false),
        // Without gst-inspect we cannot check; assume the plugins are present.
        None => true,
    }
}

pub fn missing_dependencies() -> Vec<MissingDependency> {
    let launch = ffmpeg::find_tool("gst-launch-1.0").is_some();
    let plugins = launch
        && ["pipewiresrc", "videorate", "videoconvert", "y4menc"]
            .iter()
            .all(|element| gst_has_element(element));
    if launch && plugins {
        return Vec::new();
    }
    vec![MissingDependency {
        title: "GStreamer PipeWire support is required".into(),
        detail: "On Wayland, ScreenRec receives the screen through PipeWire using GStreamer. \
                 Install the GStreamer tools and PipeWire plugin, then click Check again."
            .into(),
        install_command: linux::install_command(
            "gstreamer1.0-tools gstreamer1.0-pipewire gstreamer1.0-plugins-good",
            "gstreamer1 pipewire-gstreamer gstreamer1-plugins-good",
            "gstreamer gst-plugin-pipewire gst-plugins-good",
            "gstreamer gstreamer-plugin-pipewire gstreamer-plugins-good",
        ),
    }]
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Takes a screenshot through the portal and moves it to `output`.
pub fn screenshot(output: &Path) -> Result<()> {
    let uri = pollster::block_on(async {
        let response = Screenshot::request()
            .interactive(false)
            .modal(false)
            .send()
            .await?
            .response()?;
        ashpd::Result::Ok(response.uri().as_str().to_string())
    })
    .map_err(|err| anyhow!("the screenshot portal failed: {err}"))?;

    let path = uri
        .strip_prefix("file://")
        .map(percent_decode)
        .ok_or_else(|| anyhow!("the portal returned an unsupported location: {uri}"))?;
    let source = PathBuf::from(path);
    if std::fs::rename(&source, output).is_err() {
        std::fs::copy(&source, output).context("failed to save the screenshot")?;
        let _ = std::fs::remove_file(&source);
    }
    Ok(())
}
