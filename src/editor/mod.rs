//! A lightweight, non-destructive video editor.
//!
//! The project is a single track of clips cut from one or more source files.
//! Edits are ripple edits (no gaps): split, trim, reorder, duplicate, delete,
//! per-clip volume and mute. Everything is previewed and exported with FFmpeg.

pub mod export;
mod media;
mod player;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Margin, Rect, RichText,
    Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Vec2,
};

use self::export::{ExportClip, ExportHandle, ExportJob, ExportQuality};
use self::media::MediaInfo;
use self::player::{AudioPreview, FrameRequest, FrameWorker, VideoStream, PREVIEW_FPS};
use crate::gui::{
    button_sized, card, command_box, empty_state, field_label, file_name, icon_badge_sized,
    icon_button_sized, info_note, keycap, section_header, segmented, toggle_row, wide_button,
    ButtonKind, HoverIf, CONTROL_H, GUTTER,
};
use crate::icons::{self, Icon};
use crate::theme::pal;
use crate::util;

const MIN_CLIP: f64 = 0.1;
const HEADER_W: f32 = 96.0;
const RULER_H: f32 = 26.0;
const VIDEO_H: f32 = 62.0;
const AUDIO_H: f32 = 40.0;
const LANE_GAP: f32 = 6.0;
const PREVIEW_BOX: (u32, u32) = (1280, 720);
const STAGE: Color32 = Color32::from_rgb(12, 13, 18);

pub const VIDEO_EXTENSIONS: [&str; 9] = [
    "mp4", "mov", "mkv", "webm", "avi", "m4v", "wmv", "flv", "ts",
];

pub fn is_video(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.as_str()))
}

pub const PROJECT_EXTENSION: &str = "srproj";

pub fn is_project(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(PROJECT_EXTENSION))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ProjectFile {
    version: u32,
    sources: Vec<String>,
    clips: Vec<ProjectClip>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ProjectClip {
    source: usize,
    start: f64,
    end: f64,
    #[serde(default = "one")]
    volume: f32,
    #[serde(default)]
    muted: bool,
}

fn one() -> f32 {
    1.0
}

struct Source {
    path: PathBuf,
    name: String,
    info: Option<MediaInfo>,
    error: Option<String>,
    thumbs: Vec<(f64, TextureHandle)>,
    wave: Vec<f32>,
    add_when_ready: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Clip {
    id: u64,
    source: usize,
    start: f64,
    end: f64,
    volume: f32,
    muted: bool,
}

impl Clip {
    fn len(&self) -> f64 {
        self.end - self.start
    }
}

enum Loaded {
    Info(usize, Result<MediaInfo, String>),
    Thumbs(usize, Vec<(f64, egui::ColorImage)>),
    Wave(usize, Vec<f32>),
}

struct Playing {
    started: Instant,
    origin: f64,
    clip_id: Option<u64>,
    stream: Option<VideoStream>,
    _audio: Option<AudioPreview>,
}

#[derive(Clone, Copy)]
enum Drag {
    Trim {
        id: u64,
        left: bool,
        start: f64,
        end: f64,
        grab_x: f32,
    },
    Move {
        id: u64,
    },
    Playhead,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Resolution {
    Source,
    P1080,
    P720,
    P480,
}

impl Resolution {
    fn height(self) -> Option<u32> {
        match self {
            Resolution::Source => None,
            Resolution::P1080 => Some(1080),
            Resolution::P720 => Some(720),
            Resolution::P480 => Some(480),
        }
    }
}

enum ExportState {
    Closed,
    Settings,
    Running(ExportHandle),
    Done(Result<PathBuf, String>),
}

/// Things the editor tells the rest of the app.
pub enum EditorEvent {
    Exported(PathBuf, f64),
}

pub struct Editor {
    sources: Vec<Source>,
    clips: Vec<Clip>,
    next_id: u64,
    selected: Option<u64>,
    playhead: f64,
    playing: Option<Playing>,
    undo: Vec<Vec<Clip>>,
    redo: Vec<Vec<Clip>>,
    dirty: bool,
    project_path: Option<PathBuf>,

    pps: f32,
    scroll: f64,
    fit_pending: bool,
    drag: Option<Drag>,

    frames: FrameWorker,
    preview: Option<TextureHandle>,
    frame_key: Option<(usize, i64)>,
    frame_gen: u64,

    loader_tx: Sender<Loaded>,
    loader_rx: Receiver<Loaded>,

    export: ExportState,
    resolution: Resolution,
    export_fps: u32,
    quality: ExportQuality,

    audio_preview: bool,
    preview_muted: bool,
    tools_ok: bool,
    notice: Option<String>,
}

impl Editor {
    pub fn new(ctx: &egui::Context) -> Self {
        let (loader_tx, loader_rx) = mpsc::channel();
        Self {
            sources: Vec::new(),
            clips: Vec::new(),
            next_id: 1,
            selected: None,
            playhead: 0.0,
            playing: None,
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
            project_path: None,
            pps: 40.0,
            scroll: 0.0,
            fit_pending: true,
            drag: None,
            frames: FrameWorker::new(ctx.clone()),
            preview: None,
            frame_key: None,
            frame_gen: 0,
            loader_tx,
            loader_rx,
            export: ExportState::Closed,
            resolution: Resolution::Source,
            export_fps: 30,
            quality: ExportQuality::Balanced,
            audio_preview: AudioPreview::available(),
            preview_muted: false,
            tools_ok: crate::tools::ffmpeg().is_some() && crate::tools::ffprobe().is_some(),
            notice: None,
        }
    }

    // -----------------------------------------------------------------
    // Project
    // -----------------------------------------------------------------

    /// "My project" or "Untitled project".
    pub fn project_name(&self) -> String {
        self.project_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled project".into())
    }

    fn projects_dir() -> PathBuf {
        util::recordings_dir().join("Projects")
    }

    /// Saves the project; asks for a location the first time (or `save_as`).
    /// Returns false when the user cancelled or saving failed.
    pub fn save_project(&mut self, save_as: bool) -> bool {
        let path = match (&self.project_path, save_as) {
            (Some(path), false) => path.clone(),
            _ => {
                let dir = Self::projects_dir();
                let _ = std::fs::create_dir_all(&dir);
                let Some(mut path) = rfd::FileDialog::new()
                    .set_title("Save project")
                    .add_filter("ScreenRec project", &[PROJECT_EXTENSION])
                    .set_directory(&dir)
                    .set_file_name(format!("{}.{PROJECT_EXTENSION}", self.project_name()))
                    .save_file()
                else {
                    return false;
                };
                if path.extension().is_none() {
                    path.set_extension(PROJECT_EXTENSION);
                }
                path
            }
        };
        let file = ProjectFile {
            version: 1,
            sources: self
                .sources
                .iter()
                .map(|s| s.path.to_string_lossy().to_string())
                .collect(),
            clips: self
                .clips
                .iter()
                .map(|c| ProjectClip {
                    source: c.source,
                    start: c.start,
                    end: c.end,
                    volume: c.volume,
                    muted: c.muted,
                })
                .collect(),
        };
        let result = serde_json::to_string_pretty(&file)
            .map_err(|e| e.to_string())
            .and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()));
        match result {
            Ok(()) => {
                self.project_path = Some(path);
                self.dirty = false;
                self.notice = None;
                true
            }
            Err(err) => {
                self.notice = Some(format!("Could not save the project: {err}"));
                false
            }
        }
    }

    pub fn open_project_dialog(&mut self, ctx: &egui::Context) {
        let dir = Self::projects_dir();
        let mut dialog = rfd::FileDialog::new()
            .set_title("Open project")
            .add_filter("ScreenRec project", &[PROJECT_EXTENSION]);
        if dir.is_dir() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.open_project(&path, ctx);
        }
    }

    pub fn open_project(&mut self, path: &Path, ctx: &egui::Context) {
        let file: ProjectFile = match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
        {
            Ok(file) => file,
            Err(err) => {
                self.notice = Some(format!("Could not open {}: {err}", file_name(path)));
                return;
            }
        };
        self.stop();
        *self = Editor::new(ctx);
        for source in &file.sources {
            self.open(PathBuf::from(source), false, ctx);
        }
        self.clips = file
            .clips
            .iter()
            .filter(|c| c.source < self.sources.len() && c.end > c.start)
            .map(|c| Clip {
                id: {
                    self.next_id += 1;
                    self.next_id
                },
                source: c.source,
                start: c.start,
                end: c.end,
                volume: c.volume,
                muted: c.muted,
            })
            .collect();
        self.project_path = Some(path.to_path_buf());
        self.fit_pending = true;
    }

    /// Edits that have not been exported yet.
    pub fn has_unsaved_edits(&self) -> bool {
        self.dirty && !self.clips.is_empty()
    }

    pub fn is_exporting(&self) -> bool {
        matches!(self.export, ExportState::Running(_))
    }

    /// Imports a file into the media bin (and onto the timeline when `add`).
    pub fn open(&mut self, path: PathBuf, add: bool, ctx: &egui::Context) {
        if is_project(&path) {
            self.open_project(&path, ctx);
            return;
        }
        if let Some(index) = self.sources.iter().position(|s| s.path == path) {
            if add {
                if self.sources[index].info.is_some() {
                    self.append_clip(index);
                } else {
                    self.sources[index].add_when_ready = true;
                }
            }
            return;
        }
        let index = self.sources.len();
        self.sources.push(Source {
            name: file_name(&path),
            path: path.clone(),
            info: None,
            error: None,
            thumbs: Vec::new(),
            wave: Vec::new(),
            add_when_ready: add,
        });

        let tx = self.loader_tx.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("screenrec-import".into())
            .spawn(move || {
                let info = media::probe(&path).map_err(|e| format!("{e:#}"));
                let ok = info.clone().ok();
                let _ = tx.send(Loaded::Info(index, info));
                ctx.request_repaint();
                let Some(info) = ok else {
                    return;
                };
                if let Ok(thumbs) = media::thumbnails(&path, &info, VIDEO_H as u32 - 6) {
                    let _ = tx.send(Loaded::Thumbs(index, thumbs));
                    ctx.request_repaint();
                }
                if info.has_audio {
                    if let Ok(wave) = media::waveform(&path) {
                        let _ = tx.send(Loaded::Wave(index, wave));
                        ctx.request_repaint();
                    }
                }
            })
            .ok();
    }

    pub fn import_dialog(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Import video")
            .add_filter("Video", &VIDEO_EXTENSIONS);
        let dir = util::recordings_dir();
        if dir.is_dir() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(files) = dialog.pick_files() {
            let add = self.clips.is_empty();
            for file in files {
                self.open(file, add, ctx);
            }
        }
    }

    fn receive_loaded(&mut self, ctx: &egui::Context) {
        while let Ok(loaded) = self.loader_rx.try_recv() {
            match loaded {
                Loaded::Info(index, Ok(info)) => {
                    let source = &mut self.sources[index];
                    source.info = Some(info);
                    if std::mem::take(&mut source.add_when_ready) {
                        self.append_clip(index);
                    }
                }
                Loaded::Info(index, Err(err)) => self.sources[index].error = Some(err),
                Loaded::Thumbs(index, thumbs) => {
                    self.sources[index].thumbs = thumbs
                        .into_iter()
                        .enumerate()
                        .map(|(i, (t, image))| {
                            (
                                t,
                                ctx.load_texture(
                                    format!("thumb-{index}-{i}"),
                                    image,
                                    TextureOptions::LINEAR,
                                ),
                            )
                        })
                        .collect();
                }
                Loaded::Wave(index, wave) => self.sources[index].wave = wave,
            }
        }
    }

    fn append_clip(&mut self, source: usize) {
        let Some(info) = self.sources[source].info.clone() else {
            return;
        };
        // Adding media is not an edit worth warning about on quit.
        let dirty = self.dirty;
        self.push_undo();
        self.dirty = dirty;
        let id = self.new_id();
        self.clips.push(Clip {
            id,
            source,
            start: 0.0,
            end: info.duration,
            volume: 1.0,
            muted: false,
        });
        self.selected = Some(id);
        self.fit_pending = true;
        if self.resolution == Resolution::Source && self.clips.len() == 1 {
            self.export_fps = (info.fps.round() as u32).clamp(24, 60);
        }
    }

    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn total(&self) -> f64 {
        self.clips.iter().map(Clip::len).sum()
    }

    fn clip_start(&self, index: usize) -> f64 {
        self.clips[..index].iter().map(Clip::len).sum()
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.clips.iter().position(|c| c.id == id)
    }

    /// Clip index and source time at timeline time `t`.
    fn locate(&self, t: f64) -> Option<(usize, f64)> {
        let mut start = 0.0;
        for (i, clip) in self.clips.iter().enumerate() {
            if t < start + clip.len() {
                return Some((i, clip.start + (t - start).max(0.0)));
            }
            start += clip.len();
        }
        self.clips.last().map(|c| {
            (
                self.clips.len() - 1,
                (c.end - 1.0 / PREVIEW_FPS).max(c.start),
            )
        })
    }

    fn push_undo(&mut self) {
        self.stop();
        self.undo.push(self.clips.clone());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    fn undo(&mut self) {
        if let Some(previous) = self.undo.pop() {
            self.stop();
            self.redo.push(std::mem::replace(&mut self.clips, previous));
            self.after_edit();
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.stop();
            self.undo.push(std::mem::replace(&mut self.clips, next));
            self.after_edit();
        }
    }

    fn after_edit(&mut self) {
        self.dirty = true;
        if self.selected.is_some_and(|id| self.index_of(id).is_none()) {
            self.selected = None;
        }
        self.playhead = self.playhead.min(self.total());
        self.frame_key = None;
    }

    fn split(&mut self) {
        let Some((index, source_time)) = self.locate(self.playhead) else {
            return;
        };
        let clip = self.clips[index].clone();
        if source_time - clip.start < MIN_CLIP || clip.end - source_time < MIN_CLIP {
            self.notice = Some("Move the playhead inside a clip to split it.".into());
            return;
        }
        self.push_undo();
        let id = self.new_id();
        self.clips[index].end = source_time;
        self.clips.insert(
            index + 1,
            Clip {
                id,
                start: source_time,
                ..clip
            },
        );
        self.selected = Some(id);
        self.after_edit();
    }

    fn delete_selected(&mut self) {
        let Some(index) = self.selected.and_then(|id| self.index_of(id)) else {
            return;
        };
        self.push_undo();
        self.clips.remove(index);
        self.selected = self
            .clips
            .get(index)
            .or_else(|| self.clips.last())
            .map(|c| c.id);
        self.after_edit();
        if self.clips.is_empty() {
            self.preview = None;
        }
    }

    fn duplicate_selected(&mut self) {
        let Some(index) = self.selected.and_then(|id| self.index_of(id)) else {
            return;
        };
        self.push_undo();
        let id = self.new_id();
        let copy = Clip {
            id,
            ..self.clips[index].clone()
        };
        self.clips.insert(index + 1, copy);
        self.selected = Some(id);
        self.after_edit();
    }

    fn seek(&mut self, t: f64) {
        let was_playing = self.playing.is_some();
        self.stop();
        self.playhead = t.clamp(0.0, self.total());
        if was_playing {
            self.play();
        }
    }

    // -----------------------------------------------------------------
    // Playback
    // -----------------------------------------------------------------

    fn play(&mut self) {
        if self.clips.is_empty() {
            return;
        }
        if self.playhead >= self.total() - 0.05 {
            self.playhead = 0.0;
        }
        self.playing = Some(Playing {
            started: Instant::now(),
            origin: self.playhead,
            clip_id: None,
            stream: None,
            _audio: None,
        });
    }

    pub fn stop(&mut self) {
        if self.playing.take().is_some() {
            self.frame_key = None;
        }
    }

    fn toggle_play(&mut self) {
        if self.playing.is_some() {
            self.stop();
        } else {
            self.play();
        }
    }

    fn preview_size(&self, source: usize) -> (u32, u32) {
        self.sources[source]
            .info
            .as_ref()
            .map(|i| media::fit(i.width, i.height, PREVIEW_BOX.0, PREVIEW_BOX.1))
            .unwrap_or((1280, 720))
    }

    fn set_preview(&mut self, ctx: &egui::Context, image: egui::ColorImage) {
        match &mut self.preview {
            Some(texture) if texture.size() == image.size => {
                texture.set(image, TextureOptions::LINEAR)
            }
            _ => {
                self.preview =
                    Some(ctx.load_texture("editor-preview", image, TextureOptions::LINEAR))
            }
        }
    }

    fn drive_playback(&mut self, ctx: &egui::Context) {
        let total = self.total();
        let Some(mut playing) = self.playing.take() else {
            // Paused: keep the still frame in sync with the playhead.
            if let Some((index, source_time)) = self.locate(self.playhead) {
                let clip = &self.clips[index];
                let key = (clip.source, (source_time * PREVIEW_FPS).round() as i64);
                if self.frame_key != Some(key) {
                    self.frame_key = Some(key);
                    self.frame_gen += 1;
                    self.frames.request(FrameRequest {
                        path: self.sources[clip.source].path.clone(),
                        time: source_time,
                        size: self.preview_size(clip.source),
                        generation: self.frame_gen,
                    });
                }
            }
            if let Some((_, image)) = self.frames.poll() {
                self.set_preview(ctx, image);
            }
            return;
        };

        let t = playing.origin + playing.started.elapsed().as_secs_f64();
        if t >= total {
            self.playhead = total;
            self.frame_key = None;
            return;
        }
        self.playhead = t;
        let Some((index, source_time)) = self.locate(t) else {
            return;
        };
        let clip = self.clips[index].clone();
        if playing.clip_id != Some(clip.id) {
            let source = &self.sources[clip.source];
            let remaining = clip.end - source_time;
            let size = self.preview_size(clip.source);
            playing.stream =
                VideoStream::start(&source.path, source_time, remaining, size, ctx.clone());
            let has_audio = source.info.as_ref().is_some_and(|i| i.has_audio);
            playing._audio = (has_audio && !clip.muted && !self.preview_muted && clip.volume > 0.0)
                .then(|| AudioPreview::start(&source.path, source_time, remaining, clip.volume))
                .flatten();
            playing.clip_id = Some(clip.id);
        }
        if let Some(frame) = playing
            .stream
            .as_mut()
            .and_then(|s| s.frame_for(source_time))
        {
            self.set_preview(ctx, frame);
        }
        self.playing = Some(playing);
        ctx.request_repaint();
    }

    // -----------------------------------------------------------------
    // Keyboard
    // -----------------------------------------------------------------

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || !matches!(self.export, ExportState::Closed) {
            return;
        }
        let frame = 1.0 / PREVIEW_FPS;
        let (mods, keys) = ctx.input(|i| {
            let keys: Vec<egui::Key> = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key, pressed: true, ..
                    } => Some(*key),
                    _ => None,
                })
                .collect();
            (i.modifiers, keys)
        });
        for key in keys {
            match key {
                egui::Key::Space => self.toggle_play(),
                egui::Key::S if !mods.command => self.split(),
                egui::Key::Delete | egui::Key::Backspace => self.delete_selected(),
                egui::Key::Z if mods.command && mods.shift => self.redo(),
                egui::Key::Z if mods.command => self.undo(),
                egui::Key::Y if mods.command => self.redo(),
                egui::Key::D if mods.command => self.duplicate_selected(),
                egui::Key::S if mods.command => {
                    self.save_project(mods.shift);
                }
                egui::Key::O if mods.command => self.open_project_dialog(ctx),
                egui::Key::ArrowLeft => {
                    let step = if mods.shift { 1.0 } else { frame };
                    self.seek(self.playhead - step);
                }
                egui::Key::ArrowRight => {
                    let step = if mods.shift { 1.0 } else { frame };
                    self.seek(self.playhead + step);
                }
                egui::Key::Home => self.seek(0.0),
                egui::Key::End => self.seek(self.total()),
                egui::Key::Plus | egui::Key::Equals => self.zoom_by(1.25),
                egui::Key::Minus => self.zoom_by(0.8),
                _ => {}
            }
        }
    }

    fn zoom_by(&mut self, factor: f32) {
        self.pps = (self.pps * factor).clamp(2.0, 600.0);
    }

    // -----------------------------------------------------------------
    // UI
    // -----------------------------------------------------------------

    pub fn ui(&mut self, root: &mut egui::Ui) -> Vec<EditorEvent> {
        let ctx = root.ctx().clone();
        self.receive_loaded(&ctx);
        let mut events = Vec::new();
        self.poll_export(&mut events);
        let p = pal();

        if !self.tools_ok {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::same(40)))
                .show(root, |ui| self.setup_view(ui));
            return events;
        }

        self.handle_keys(&ctx);
        self.drive_playback(&ctx);

        // Columns: GUTTER | media | GUTTER | viewer | GUTTER | properties | GUTTER.
        // Every column starts with a CONTROL_H header row, so cards, stage and
        // properties all begin on the same line.
        let gutter = GUTTER as i8;
        let half = (GUTTER / 2.0) as i8;
        let narrow = ctx.content_rect().width() < 1200.0;
        let (media_w, props_w) = if narrow {
            (232.0, 264.0)
        } else {
            (256.0, 288.0)
        };

        egui::Panel::bottom("editor_timeline")
            .exact_size(AUDIO_H + VIDEO_H + RULER_H + LANE_GAP * 2.0 + 68.0)
            .frame(
                egui::Frame::new()
                    .fill(p.panel_top)
                    .stroke(Stroke::new(1.0, p.border_soft))
                    .inner_margin(Margin::symmetric(gutter, 8)),
            )
            .show_separator_line(false)
            .show(root, |ui| self.timeline(ui));

        egui::Panel::left("editor_media")
            .exact_size(media_w)
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin {
                left: gutter,
                right: half,
                top: 12,
                bottom: 12,
            }))
            .show_separator_line(false)
            .show(root, |ui| self.media_bin(ui));

        egui::Panel::right("editor_inspector")
            .exact_size(props_w)
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin {
                left: half,
                right: 0,
                top: 12,
                bottom: 12,
            }))
            .show_separator_line(false)
            .show(root, |ui| {
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width() - GUTTER, CONTROL_H),
                    egui::Layout::left_to_right(Align::Center),
                    |ui| panel_title(ui, "Properties"),
                );
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .id_salt("inspector_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width() - GUTTER);
                        self.inspector(ui);
                    });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(half, 12)),
            )
            .show(root, |ui| self.viewer(ui));

        self.export_dialog(&ctx);
        events
    }

    fn setup_view(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        ui.vertical_centered(|ui| {
            ui.set_max_width(520.0);
            ui.add_space(30.0);
            card(ui, |ui| {
                ui.with_layout(egui::Layout::top_down(Align::Min), |ui| {
                    ui.horizontal(|ui| {
                        icon_badge_sized(ui, Icon::Edit, p.accent_hi, p.accent_soft, 40.0);
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new("Set up the editor")
                                    .size(16.0)
                                    .family(crate::theme::bold())
                                    .color(p.text),
                            );
                            ui.label(
                                RichText::new("One quick install and you can edit any recording.")
                                    .size(12.0)
                                    .color(p.muted),
                            );
                        });
                    });
                    ui.add_space(12.0);
                    let missing = crate::backend::ffmpeg_missing();
                    ui.add(
                        egui::Label::new(
                            RichText::new(&missing.detail).size(12.5).color(p.text_soft),
                        )
                        .halign(Align::Min)
                        .wrap(),
                    );
                    if let Some(command) = &missing.install_command {
                        ui.add_space(8.0);
                        command_box(ui, command);
                    }
                    ui.add_space(12.0);
                    if wide_button(
                        ui,
                        Some(Icon::Refresh),
                        "Check again",
                        ButtonKind::Primary,
                        36.0,
                    )
                    .clicked()
                    {
                        self.tools_ok =
                            crate::tools::ffmpeg().is_some() && crate::tools::ffprobe().is_some();
                        self.audio_preview = AudioPreview::available();
                        if !self.tools_ok {
                            self.notice = Some(
                                "FFmpeg still wasn't found. You may need to restart ScreenRec."
                                    .into(),
                            );
                        }
                    }
                    if let Some(notice) = &self.notice {
                        ui.add_space(6.0);
                        info_note(ui, Icon::Info, notice, p.caution);
                    }
                });
            });
        });
    }

    fn media_bin(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let ctx = ui.ctx().clone();
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), CONTROL_H),
            egui::Layout::left_to_right(Align::Center),
            |ui| {
                panel_title(ui, "Media");
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if button_sized(
                        ui,
                        Some(Icon::Import),
                        "Import",
                        ButtonKind::Primary,
                        96.0,
                        CONTROL_H,
                    )
                    .on_hover_text("Import video files")
                    .clicked()
                    {
                        self.import_dialog(&ctx);
                    }
                });
            },
        );
        ui.add_space(8.0);

        if self.sources.is_empty() {
            drop_zone(ui);
            return;
        }

        let mut add = None;
        egui::ScrollArea::vertical()
            .id_salt("media_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (index, source) in self.sources.iter().enumerate() {
                    let used = self.clips.iter().any(|c| c.source == index);
                    let response = egui::Frame::new()
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0, if used { p.border_hi } else { p.border }))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(8))
                        .shadow(p.card_shadow)
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            let (thumb_rect, _) = ui.allocate_exact_size(
                                vec2(ui.available_width(), 116.0),
                                Sense::hover(),
                            );
                            ui.painter()
                                .rect_filled(thumb_rect, CornerRadius::same(4), STAGE);
                            if let Some((_, texture)) = source.thumbs.get(source.thumbs.len() / 3) {
                                let size = fit_size(texture.size_vec2(), thumb_rect.size());
                                ui.painter().image(
                                    texture.id(),
                                    Rect::from_center_size(thumb_rect.center(), size),
                                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            } else if source.error.is_none() {
                                ui.put(
                                    Rect::from_center_size(thumb_rect.center(), Vec2::splat(22.0)),
                                    egui::Spinner::new().color(p.accent_hi),
                                );
                            }
                            if let Some(info) = &source.info {
                                let text = fmt_time(info.duration, false);
                                let galley = ui.painter().layout_no_wrap(
                                    text,
                                    FontId::proportional(11.5),
                                    Color32::WHITE,
                                );
                                let tag = Rect::from_min_size(
                                    thumb_rect.right_bottom() - galley.size() - vec2(14.0, 10.0),
                                    galley.size() + vec2(8.0, 4.0),
                                );
                                ui.painter().rect_filled(
                                    tag,
                                    CornerRadius::same(3),
                                    Color32::from_black_alpha(170),
                                );
                                ui.painter().galley(
                                    tag.min + vec2(4.0, 2.0),
                                    galley,
                                    Color32::WHITE,
                                );
                            }
                            ui.add_space(4.0);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&source.name).size(12.5).color(p.text),
                                )
                                .truncate(),
                            );
                            ui.horizontal(|ui| {
                                let detail = match (&source.info, &source.error) {
                                    (Some(info), _) => format!(
                                        "{}×{} · {:.0} fps{}",
                                        info.width,
                                        info.height,
                                        info.fps,
                                        if info.has_audio { "" } else { " · no audio" }
                                    ),
                                    (None, Some(_)) => "Unsupported file".into(),
                                    _ => "Reading…".into(),
                                };
                                ui.label(RichText::new(detail).size(11.5).color(
                                    if source.error.is_some() {
                                        p.danger_text
                                    } else {
                                        p.muted
                                    },
                                ));
                                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                    if source.info.is_some()
                                        && icon_button_sized(
                                            ui,
                                            Icon::Plus,
                                            "Add to end of timeline",
                                            26.0,
                                        )
                                        .clicked()
                                    {
                                        add = Some(index);
                                    }
                                });
                            });
                        })
                        .response;
                    if let Some(err) = &source.error {
                        response.on_hover_text(err);
                    } else {
                        response.on_hover_text(source.path.display().to_string());
                    }
                    ui.add_space(8.0);
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Tip: drop video files anywhere in the window to import them.")
                        .size(11.5)
                        .color(p.muted_dim),
                );
            });
        if let Some(index) = add {
            self.append_clip(index);
        }
    }

    fn viewer(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let total = self.total();

        // Toolbar
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), CONTROL_H),
            egui::Layout::left_to_right(Align::Center),
            |ui| {
                let ctx = ui.ctx().clone();
                if icon_button_sized(ui, Icon::Folder, "Open project (Ctrl+O)", 32.0).clicked() {
                    self.open_project_dialog(&ctx);
                }
                ui.add_enabled_ui(!self.clips.is_empty(), |ui| {
                    if icon_button_sized(ui, Icon::Save, "Save project (Ctrl+S)", 32.0).clicked() {
                        self.save_project(false);
                    }
                });
                ui.add_space(4.0);
                let title = format!(
                    "{}{}",
                    self.project_name(),
                    if self.dirty && !self.clips.is_empty() {
                        "*"
                    } else {
                        ""
                    }
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(title)
                            .size(14.5)
                            .family(crate::theme::bold())
                            .color(p.text),
                    )
                    .truncate(),
                );
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    let enabled = !self.clips.is_empty() && !self.is_exporting();
                    ui.add_enabled_ui(enabled, |ui| {
                        if button_sized(
                            ui,
                            Some(Icon::Export),
                            "Export",
                            ButtonKind::Primary,
                            104.0,
                            32.0,
                        )
                        .on_hover_text("Render the timeline to an MP4 file")
                        .clicked()
                        {
                            self.stop();
                            self.export = ExportState::Settings;
                        }
                    });
                });
            },
        );
        ui.add_space(8.0);

        // Stage
        let transport_h = 52.0;
        let stage = ui.available_rect_before_wrap();
        let stage = Rect::from_min_max(stage.min, pos2(stage.max.x, stage.max.y - transport_h));
        let (stage_rect, stage_resp) = ui.allocate_exact_size(stage.size(), Sense::click());
        ui.painter()
            .rect_filled(stage_rect, CornerRadius::same(6), STAGE);

        if self.clips.is_empty() {
            let mut import = false;
            let inner = Rect::from_center_size(stage_rect.center(), vec2(320.0, 170.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
                ui.vertical_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(46.0), Sense::hover());
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(6),
                        Color32::from_rgb(30, 31, 44),
                    );
                    icons::paint(
                        ui.painter(),
                        rect.shrink(12.0),
                        Icon::Film,
                        Color32::from_rgb(160, 162, 255),
                    );
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Start editing")
                            .size(15.5)
                            .family(crate::theme::bold())
                            .color(Color32::WHITE),
                    );
                    ui.label(
                        RichText::new(
                            "Import a video, drop one here, or edit a recording from Recent.",
                        )
                        .size(12.0)
                        .color(Color32::from_rgb(160, 165, 182)),
                    );
                    ui.add_space(10.0);
                    if button_sized(
                        ui,
                        Some(Icon::Import),
                        "Import video",
                        ButtonKind::Primary,
                        150.0,
                        34.0,
                    )
                    .clicked()
                    {
                        import = true;
                    }
                });
            });
            if import {
                self.import_dialog(ui.ctx());
            }
        } else if let Some(texture) = &self.preview {
            let size = fit_size(texture.size_vec2(), stage_rect.shrink(10.0).size());
            ui.painter().image(
                texture.id(),
                Rect::from_center_size(stage_rect.center(), size),
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            ui.put(
                Rect::from_center_size(stage_rect.center(), Vec2::splat(28.0)),
                egui::Spinner::new().color(p.accent_hi),
            );
        }
        if stage_resp.clicked() && !self.clips.is_empty() {
            self.toggle_play();
        }

        // Transport
        ui.add_space(8.0);
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
        let enabled = !self.clips.is_empty();
        let time_text = format!(
            "{} / {}",
            fmt_time(self.playhead, true),
            fmt_time(total, true)
        );
        let time_font = FontId::monospace(12.0);
        let time_w = ui
            .painter()
            .layout_no_wrap(time_text.clone(), time_font.clone(), p.text_soft)
            .size()
            .x;
        ui.painter().text(
            pos2(row.left() + 2.0, row.center().y),
            Align2::LEFT_CENTER,
            time_text,
            time_font,
            if enabled { p.text_soft } else { p.muted_dim },
        );

        // Playback controls sit exactly under the centre of the stage; if the
        // stage is too narrow, they move right of the time readout instead.
        let gap = 4.0;
        let controls_w = 4.0 * 34.0 + 40.0 + 4.0 * gap;
        let centered = row.center().x - controls_w / 2.0;
        let left = centered.max(row.left() + time_w + 12.0);
        let controls = Rect::from_min_size(pos2(left, row.top()), vec2(controls_w, row.height()));
        let mut transport = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(controls)
                .layout(egui::Layout::left_to_right(Align::Center)),
        );
        transport.spacing_mut().item_spacing.x = gap;
        transport.add_enabled_ui(enabled, |ui| {
            if icon_button_sized(ui, Icon::SkipStart, "Go to start (Home)", 34.0).clicked() {
                self.seek(0.0);
            }
            if icon_button_sized(ui, Icon::StepBack, "Previous frame (←)", 34.0).clicked() {
                self.seek(self.playhead - 1.0 / PREVIEW_FPS);
            }
            let playing = self.playing.is_some();
            if play_button(ui, playing).clicked() {
                self.toggle_play();
            }
            if icon_button_sized(ui, Icon::StepForward, "Next frame (→)", 34.0).clicked() {
                self.seek(self.playhead + 1.0 / PREVIEW_FPS);
            }
            if icon_button_sized(ui, Icon::SkipEnd, "Go to end (End)", 34.0).clicked() {
                self.seek(total);
            }
        });

        let mute_rect = Rect::from_min_max(pos2(row.right() - 34.0, row.top()), row.max);
        let mut right = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(mute_rect)
                .layout(egui::Layout::right_to_left(Align::Center)),
        );
        right.add_enabled_ui(enabled, |ui| {
                    let (icon, tip) = if self.preview_muted {
                        (Icon::Mute, "Unmute preview")
                    } else {
                        (Icon::Speaker, "Mute preview")
                    };
                    if self.audio_preview {
                        if icon_button_sized(ui, icon, tip, 32.0).clicked() {
                            self.preview_muted = !self.preview_muted;
                            if self.playing.is_some() {
                                let t = self.playhead;
                                self.seek(t);
                            }
                        }
                    } else {
                        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
                        icons::paint(ui.painter(), rect.shrink(9.0), Icon::Mute, p.muted_dim);
                        resp.on_hover_text("Audio preview needs ffplay (included with most FFmpeg installs). Exports still include audio.");
                    }
        });
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let selected = self.selected.and_then(|id| self.index_of(id));
        if let Some(index) = selected {
            let mut clip = self.clips[index].clone();
            let source = &self.sources[clip.source];
            let duration = source.info.as_ref().map(|i| i.duration).unwrap_or(clip.end);
            let name = source.name.clone();
            let mut action = None;
            card(ui, |ui| {
                section_header(
                    ui,
                    Icon::Film,
                    "Clip",
                    &format!("{} of {}", index + 1, self.clips.len()),
                );
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(RichText::new(&name).size(12.0).color(p.text_soft)).truncate(),
                );
                ui.add_space(8.0);

                field_label(ui, Some(Icon::Timer), "Trim (seconds in source)");
                let (start_max, end_min) = (clip.end - MIN_CLIP, clip.start + MIN_CLIP);
                ui.columns(2, |columns| {
                    for (column, (label, value, range)) in columns.iter_mut().zip([
                        ("In", &mut clip.start, 0.0..=start_max),
                        ("Out", &mut clip.end, end_min..=duration),
                    ]) {
                        column.spacing_mut().item_spacing.y = 3.0;
                        column.label(RichText::new(label).size(11.5).color(p.muted_dim));
                        column.add_sized(
                            vec2(column.available_width(), CONTROL_H),
                            egui::DragValue::new(value)
                                .range(range)
                                .speed(0.05)
                                .fixed_decimals(2)
                                .suffix(" s"),
                        );
                    }
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Length").size(12.0).color(p.muted));
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(fmt_time(clip.len(), true))
                                .size(12.0)
                                .color(p.text),
                        );
                    });
                });

                ui.add_space(10.0);
                field_label(ui, Some(Icon::Speaker), "Volume");
                let mut percent = clip.volume * 100.0;
                ui.spacing_mut().slider_width = (ui.available_width() - 64.0).max(60.0);
                ui.add_enabled(
                    !clip.muted,
                    egui::Slider::new(&mut percent, 0.0..=200.0)
                        .suffix("%")
                        .fixed_decimals(0),
                );
                clip.volume = percent / 100.0;
                toggle_row(
                    ui,
                    Icon::Mute,
                    "Mute clip",
                    "Silence this clip",
                    &mut clip.muted,
                );

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let half = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                    if button_sized(
                        ui,
                        Some(Icon::Scissors),
                        "Split",
                        ButtonKind::Secondary,
                        half,
                        32.0,
                    )
                    .on_hover_text("Split at playhead (S)")
                    .clicked()
                    {
                        action = Some(0);
                    }
                    if button_sized(
                        ui,
                        Some(Icon::Duplicate),
                        "Duplicate",
                        ButtonKind::Secondary,
                        half,
                        32.0,
                    )
                    .on_hover_text("Duplicate clip (Ctrl+D)")
                    .clicked()
                    {
                        action = Some(1);
                    }
                });
                ui.add_space(6.0);
                if wide_button(
                    ui,
                    Some(Icon::Trash),
                    "Delete clip",
                    ButtonKind::Danger,
                    32.0,
                )
                .on_hover_text("Delete (Del)")
                .clicked()
                {
                    action = Some(2);
                }
            });

            if clip != self.clips[index] {
                // Group continuous slider/drag changes into one undo step.
                let dragging = ui.ctx().input(|i| i.pointer.any_down());
                if !dragging || self.undo.last().is_none_or(|last| last != &self.clips) {
                    self.undo.push(self.clips.clone());
                    self.redo.clear();
                }
                self.clips[index] = clip;
                self.after_edit();
                if self.playing.is_some() {
                    self.stop();
                }
            }
            match action {
                Some(0) => self.split(),
                Some(1) => self.duplicate_selected(),
                Some(2) => self.delete_selected(),
                _ => {}
            }
            ui.add_space(12.0);
        } else {
            card(ui, |ui| {
                section_header(ui, Icon::Sliders, "Project", "Select a clip to edit it");
                ui.add_space(10.0);
                let first = self
                    .clips
                    .first()
                    .and_then(|c| self.sources[c.source].info.clone());
                project_row(ui, "Clips", &self.clips.len().to_string());
                project_row(ui, "Length", &fmt_time(self.total(), true));
                if let Some(info) = first {
                    project_row(ui, "Canvas", &format!("{}×{}", info.width, info.height));
                    project_row(ui, "Frame rate", &format!("{:.0} fps", info.fps));
                }
            });
            ui.add_space(12.0);
        }

        card(ui, |ui| {
            section_header(ui, Icon::Keyboard, "Shortcuts", "Work faster");
            ui.add_space(8.0);
            for (key, what) in [
                ("Space", "Play / pause"),
                ("S", "Split at playhead"),
                ("Del", "Delete clip"),
                ("← →", "Step one frame"),
                ("Shift ← →", "Jump one second"),
                ("Ctrl Z", "Undo"),
                ("Ctrl Y", "Redo"),
                ("Ctrl D", "Duplicate clip"),
                ("+ −", "Zoom timeline"),
            ] {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(what).size(12.0).color(p.text_soft));
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        keycap(ui, key)
                    });
                });
            }
        });
    }

    // -----------------------------------------------------------------
    // Timeline
    // -----------------------------------------------------------------

    fn timeline(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        // Toolbar
        ui.horizontal(|ui| {
            ui.set_height(32.0);
            ui.add_enabled_ui(!self.undo.is_empty(), |ui| {
                if icon_button_sized(ui, Icon::Undo, "Undo (Ctrl+Z)", 30.0).clicked() {
                    self.undo();
                }
            });
            ui.add_enabled_ui(!self.redo.is_empty(), |ui| {
                if icon_button_sized(ui, Icon::Redo, "Redo (Ctrl+Y)", 30.0).clicked() {
                    self.redo();
                }
            });
            separator(ui);
            ui.add_enabled_ui(!self.clips.is_empty(), |ui| {
                if icon_button_sized(ui, Icon::Scissors, "Split at playhead (S)", 30.0).clicked() {
                    self.split();
                }
            });
            ui.add_enabled_ui(self.selected.is_some(), |ui| {
                if icon_button_sized(ui, Icon::Duplicate, "Duplicate clip (Ctrl+D)", 30.0).clicked()
                {
                    self.duplicate_selected();
                }
                if icon_button_sized(ui, Icon::Trash, "Delete clip (Del)", 30.0).clicked() {
                    self.delete_selected();
                }
            });
            if let Some(notice) = &self.notice {
                ui.add_space(8.0);
                ui.label(RichText::new(notice).size(12.0).color(p.caution));
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if icon_button_sized(ui, Icon::Fit, "Fit timeline to window", 30.0).clicked() {
                    self.fit_pending = true;
                }
                if icon_button_sized(ui, Icon::Plus, "Zoom in (+)", 30.0).clicked() {
                    self.zoom_by(1.25);
                }
                ui.spacing_mut().slider_width = 110.0;
                let mut zoom = self.pps.ln();
                if ui
                    .add(egui::Slider::new(&mut zoom, 2f32.ln()..=600f32.ln()).show_value(false))
                    .on_hover_text("Timeline zoom")
                    .changed()
                {
                    self.pps = zoom.exp();
                }
                if icon_button_sized(ui, Icon::Minus, "Zoom out (−)", 30.0).clicked() {
                    self.zoom_by(0.8);
                }
            });
        });
        ui.add_space(4.0);

        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let total = self.total();
        let x0 = rect.left() + HEADER_W;
        let area_w = (rect.right() - x0 - 12.0).max(50.0);

        if self.fit_pending && total > 0.0 {
            self.pps = (area_w / total as f32).clamp(2.0, 600.0);
            self.scroll = 0.0;
            self.fit_pending = false;
        }

        // Wheel: pan, Ctrl+wheel: zoom around the pointer.
        if response.hovered() {
            let (scroll, zoom, pointer) =
                ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
            if zoom != 1.0 {
                let anchor = pointer.map(|p| self.scroll + ((p.x - x0) / self.pps) as f64);
                self.pps = (self.pps * zoom).clamp(2.0, 600.0);
                if let (Some(anchor), Some(pointer)) = (anchor, pointer) {
                    self.scroll = anchor - ((pointer.x - x0) / self.pps) as f64;
                }
            } else if scroll != Vec2::ZERO {
                let delta = if scroll.x.abs() > scroll.y.abs() {
                    scroll.x
                } else {
                    scroll.y
                };
                self.scroll -= (delta / self.pps) as f64;
            }
        }
        let max_scroll = (total - (area_w * 0.5 / self.pps) as f64).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);

        let pps = self.pps;
        let scroll = self.scroll;
        let to_x = |t: f64| x0 + ((t - scroll) as f32) * pps;
        let to_t = |x: f32| scroll + ((x - x0) / pps) as f64;

        let ruler = Rect::from_min_max(
            pos2(x0, rect.top()),
            pos2(rect.right(), rect.top() + RULER_H),
        );
        let video_lane = Rect::from_min_size(
            pos2(rect.left(), ruler.bottom() + LANE_GAP),
            vec2(rect.width(), VIDEO_H),
        );
        let audio_lane = Rect::from_min_size(
            pos2(rect.left(), video_lane.bottom() + LANE_GAP),
            vec2(rect.width(), AUDIO_H),
        );

        // Track headers
        for (lane, icon, name) in [
            (video_lane, Icon::Film, "Video"),
            (audio_lane, Icon::Speaker, "Audio"),
        ] {
            let header = Rect::from_min_size(lane.min, vec2(HEADER_W - 8.0, lane.height()));
            painter.rect_filled(header, CornerRadius::same(4), p.surface_elevated);
            icons::paint(
                &painter,
                Rect::from_center_size(
                    pos2(header.left() + 18.0, header.center().y),
                    Vec2::splat(14.0),
                ),
                icon,
                p.muted,
            );
            painter.text(
                pos2(header.left() + 32.0, header.center().y),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(12.0),
                p.text_soft,
            );
            painter.rect_filled(
                Rect::from_min_max(pos2(x0, lane.top()), lane.max),
                CornerRadius::same(4),
                p.input_bg,
            );
        }

        // Ruler
        let step = [
            0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0,
        ]
        .into_iter()
        .find(|s| (*s as f32) * pps >= 84.0)
        .unwrap_or(3600.0);
        let minor = step / 5.0;
        let first = (scroll / minor).floor() as i64;
        let last = (to_t(rect.right()) / minor).ceil() as i64;
        for k in first..=last {
            let t = k as f64 * minor;
            if t < 0.0 {
                continue;
            }
            let x = to_x(t);
            let major = (k % 5) == 0;
            painter.line_segment(
                [
                    pos2(x, ruler.bottom() - if major { 9.0 } else { 4.0 }),
                    pos2(x, ruler.bottom()),
                ],
                Stroke::new(1.0, if major { p.muted } else { p.border_hi }),
            );
            if major {
                painter.text(
                    pos2(x + 6.0, ruler.top() + 2.0),
                    Align2::LEFT_TOP,
                    fmt_time(t, step < 1.0),
                    FontId::proportional(11.5),
                    p.muted,
                );
            }
        }

        // Clips
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let mut starts = Vec::with_capacity(self.clips.len());
        let mut acc = 0.0;
        for clip in &self.clips {
            starts.push(acc);
            acc += clip.len();
        }

        let mut clicked_clip = None;
        let mut new_drag = None;
        for (i, clip) in self.clips.iter().enumerate() {
            let x_a = to_x(starts[i]);
            let x_b = to_x(starts[i] + clip.len());
            if x_b < x0 || x_a > rect.right() {
                continue;
            }
            let video_rect = Rect::from_min_max(
                pos2(x_a + 1.0, video_lane.top()),
                pos2(x_b - 1.0, video_lane.bottom()),
            );
            let audio_rect = Rect::from_min_max(
                pos2(x_a + 1.0, audio_lane.top()),
                pos2(x_b - 1.0, audio_lane.bottom()),
            );
            let body = video_rect.union(audio_rect);
            let id = ui.scope_id().with(("clip", clip.id));
            let body_resp = ui.interact(body.intersect(rect), id, Sense::click_and_drag());
            let handle_w = 7.0f32.min(video_rect.width() / 3.0);
            let left_resp = ui.interact(
                Rect::from_min_size(body.min, vec2(handle_w, body.height())).intersect(rect),
                id.with("l"),
                Sense::drag(),
            );
            let right_resp = ui.interact(
                Rect::from_min_size(
                    pos2(body.right() - handle_w, body.top()),
                    vec2(handle_w, body.height()),
                )
                .intersect(rect),
                id.with("r"),
                Sense::drag(),
            );
            if left_resp.hovered()
                || right_resp.hovered()
                || left_resp.dragged()
                || right_resp.dragged()
            {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            } else if body_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::Grab);
            }
            for (resp, left) in [(&left_resp, true), (&right_resp, false)] {
                if resp.drag_started() {
                    new_drag = Some(Drag::Trim {
                        id: clip.id,
                        left,
                        start: clip.start,
                        end: clip.end,
                        grab_x: pointer.map(|p| p.x).unwrap_or(0.0),
                    });
                }
            }
            if body_resp.drag_started() && new_drag.is_none() {
                new_drag = Some(Drag::Move { id: clip.id });
            }
            if body_resp.clicked() {
                clicked_clip = Some(clip.id);
            }

            let selected = self.selected == Some(clip.id);
            let moving = matches!(self.drag, Some(Drag::Move { id }) if id == clip.id);
            let source = &self.sources[clip.source];
            let clip_painter = painter.with_clip_rect(video_rect.intersect(rect));

            // Video lane: thumbnail strip.
            clip_painter.rect_filled(
                video_rect,
                CornerRadius::same(4),
                Color32::from_rgb(34, 35, 60),
            );
            if let Some((_, first_tex)) = source.thumbs.first() {
                let tile = first_tex.size_vec2() * (video_rect.height() / first_tex.size_vec2().y);
                let mut x = video_rect.left();
                while x < video_rect.right() && tile.x > 4.0 {
                    if x + tile.x >= x0 && x <= rect.right() {
                        let t = clip.start + ((x - video_rect.left() + tile.x / 2.0) / pps) as f64;
                        let texture = nearest_thumb(&source.thumbs, t);
                        clip_painter.image(
                            texture.id(),
                            Rect::from_min_size(pos2(x, video_rect.top()), tile),
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::from_white_alpha(if moving { 120 } else { 255 }),
                        );
                    }
                    x += tile.x;
                }
            }
            // Name tag
            let label = format!("{}  ·  {}", source.name, fmt_time(clip.len(), false));
            let galley = painter.layout_no_wrap(label, FontId::proportional(11.5), Color32::WHITE);
            let tag = Rect::from_min_size(
                video_rect.min + vec2(6.0, 5.0),
                galley.size() + vec2(10.0, 4.0),
            );
            if tag.width() < video_rect.width() - 8.0 {
                clip_painter.rect_filled(
                    tag,
                    CornerRadius::same(3),
                    Color32::from_black_alpha(160),
                );
                clip_painter.galley(tag.min + vec2(5.0, 2.0), galley, Color32::WHITE);
            }

            // Audio lane: waveform.
            let audio_painter = painter.with_clip_rect(audio_rect.intersect(rect));
            let muted = clip.muted || source.info.as_ref().is_some_and(|i| !i.has_audio);
            audio_painter.rect_filled(
                audio_rect,
                CornerRadius::same(4),
                if muted {
                    p.surface_elevated
                } else {
                    p.accent_soft
                },
            );
            if !source.wave.is_empty() && !clip.muted {
                let mid = audio_rect.center().y;
                let half = audio_rect.height() / 2.0 - 4.0;
                let wave_color = p.accent_hi.gamma_multiply(0.85);
                let mut x = audio_rect.left().max(x0);
                let end = audio_rect.right().min(rect.right());
                while x < end {
                    let t = clip.start + ((x - audio_rect.left()) / pps) as f64;
                    let i = (t * media::WAVE_RATE) as usize;
                    let peak = (source.wave.get(i).copied().unwrap_or(0.0) * clip.volume).min(1.0);
                    let h = (peak * half).max(0.5);
                    audio_painter.line_segment(
                        [pos2(x, mid - h), pos2(x, mid + h)],
                        Stroke::new(1.0, wave_color),
                    );
                    x += 2.0;
                }
            } else if muted {
                audio_painter.text(
                    audio_rect.left_center() + vec2(8.0, 0.0),
                    Align2::LEFT_CENTER,
                    if clip.muted { "Muted" } else { "No audio" },
                    FontId::proportional(11.5),
                    p.muted_dim,
                );
            }

            // Borders / selection
            let border = if selected {
                Stroke::new(2.0, p.accent)
            } else {
                Stroke::new(1.0, p.border_hi)
            };
            for r in [video_rect, audio_rect] {
                painter.rect_stroke(
                    r.intersect(rect),
                    CornerRadius::same(4),
                    border,
                    StrokeKind::Inside,
                );
            }
            if selected || body_resp.hovered() {
                for r in [
                    Rect::from_min_size(
                        video_rect.min + vec2(2.0, 18.0),
                        vec2(3.0, video_rect.height() - 36.0),
                    ),
                    Rect::from_min_size(
                        pos2(video_rect.right() - 5.0, video_rect.top() + 18.0),
                        vec2(3.0, video_rect.height() - 36.0),
                    ),
                ] {
                    painter.rect_filled(r, CornerRadius::same(3), Color32::from_white_alpha(200));
                }
            }
        }

        if let Some(drag) = new_drag {
            if matches!(drag, Drag::Trim { .. }) {
                self.push_undo();
            }
            if let Drag::Trim { id, .. } | Drag::Move { id } = drag {
                self.selected = Some(id);
            }
            self.drag = Some(drag);
        }

        // Empty-area / ruler interaction: seek and scrub.
        if self.drag.is_none() && response.drag_started() {
            self.drag = Some(Drag::Playhead);
            self.stop();
        }
        if response.clicked() && clicked_clip.is_none() {
            if let Some(pos) = pointer {
                if pos.x >= x0 {
                    self.seek(to_t(pos.x));
                }
                if pos.y > ruler.bottom() && to_t(pos.x) > total {
                    self.selected = None;
                }
            }
        }
        if let Some(id) = clicked_clip {
            self.selected = Some(id);
            if let Some(pos) = pointer {
                self.seek(to_t(pos.x));
            }
        }

        // Apply drags
        let released = ui.input(|i| i.pointer.any_released());
        match self.drag {
            Some(Drag::Playhead) => {
                if let Some(pos) = pointer {
                    self.playhead = to_t(pos.x).clamp(0.0, total);
                }
            }
            Some(Drag::Trim {
                id,
                left,
                start,
                end,
                grab_x,
            }) => {
                if let (Some(index), Some(pos)) = (self.index_of(id), pointer) {
                    let duration = self.sources[self.clips[index].source]
                        .info
                        .as_ref()
                        .map(|i| i.duration)
                        .unwrap_or(end);
                    let dt = ((pos.x - grab_x) / pps) as f64;
                    if left {
                        self.clips[index].start = (start + dt).clamp(0.0, end - MIN_CLIP);
                        self.playhead = self.clip_start(index);
                    } else {
                        self.clips[index].end = (end + dt).clamp(start + MIN_CLIP, duration);
                        self.playhead =
                            self.clip_start(index) + self.clips[index].len() - 1.0 / PREVIEW_FPS;
                    }
                    self.frame_key = None;
                }
            }
            Some(Drag::Move { id }) => {
                if let (Some(index), Some(pos)) = (self.index_of(id), pointer) {
                    let target = drop_index(&starts, &self.clips, to_t(pos.x), index);
                    if target != index {
                        let x = to_x(target_insert_time(&starts, &self.clips, target, index));
                        painter.line_segment(
                            [
                                pos2(x, video_lane.top() - 3.0),
                                pos2(x, audio_lane.bottom() + 3.0),
                            ],
                            Stroke::new(3.0, p.accent),
                        );
                    }
                    if released && target != index {
                        self.push_undo();
                        let clip = self.clips.remove(index);
                        let target = if target > index { target - 1 } else { target };
                        self.clips.insert(target, clip);
                        self.playhead = self.clip_start(target);
                        self.after_edit();
                    }
                }
            }
            None => {}
        }
        if released && self.drag.is_some() {
            if matches!(self.drag, Some(Drag::Trim { .. })) {
                self.after_edit();
            }
            self.drag = None;
        }

        // Playhead
        let x = to_x(self.playhead);
        if x >= x0 - 1.0 && x <= rect.right() {
            painter.line_segment(
                [
                    pos2(x, ruler.top() + 4.0),
                    pos2(x, audio_lane.bottom() + 4.0),
                ],
                Stroke::new(2.0, p.danger),
            );
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(x - 6.0, ruler.top()),
                    pos2(x + 6.0, ruler.top()),
                    pos2(x + 6.0, ruler.top() + 8.0),
                    pos2(x, ruler.top() + 14.0),
                    pos2(x - 6.0, ruler.top() + 8.0),
                ],
                p.danger,
                Stroke::NONE,
            ));
        }

        if self.clips.is_empty() {
            painter.text(
                pos2(x0 + 16.0, video_lane.center().y),
                Align2::LEFT_CENTER,
                "Your timeline is empty. Add a video from Media.",
                FontId::proportional(12.5),
                p.muted,
            );
        }
    }

    // -----------------------------------------------------------------
    // Export
    // -----------------------------------------------------------------

    fn poll_export(&mut self, events: &mut Vec<EditorEvent>) {
        if let ExportState::Running(handle) = &self.export {
            if let Ok(result) = handle.done.try_recv() {
                if let Ok(path) = &result {
                    events.push(EditorEvent::Exported(path.clone(), self.total()));
                }
                self.export = ExportState::Done(result);
            }
        }
    }

    fn project_size(&self) -> (u32, u32) {
        let (w, h) = self
            .clips
            .first()
            .and_then(|c| self.sources[c.source].info.as_ref())
            .map(|i| (i.width.max(2), i.height.max(2)))
            .unwrap_or((1920, 1080));
        match self.resolution.height() {
            Some(target) if target < h => {
                let width = (w as f64 * target as f64 / h as f64).round() as u32;
                (width / 2 * 2, target)
            }
            _ => (w / 2 * 2, h / 2 * 2),
        }
    }

    fn start_export(&mut self, ctx: &egui::Context) {
        let Some(first) = self
            .clips
            .first()
            .map(|c| self.sources[c.source].path.clone())
        else {
            return;
        };
        let suggested = export::default_output(&first);
        let mut dialog = rfd::FileDialog::new()
            .set_title("Export video")
            .add_filter("MP4 video", &["mp4"])
            .set_file_name(file_name(&suggested));
        if let Some(dir) = suggested.parent().filter(|d| d.is_dir()) {
            dialog = dialog.set_directory(dir);
        }
        let Some(mut output) = dialog.save_file() else {
            return;
        };
        if output.extension().is_none() {
            output.set_extension("mp4");
        }
        if self.sources.iter().any(|s| s.path == output) {
            self.export = ExportState::Done(Err(
                "Choose a different file name; this one is used by a clip in the project.".into(),
            ));
            return;
        }

        let (width, height) = self.project_size();
        let job = ExportJob {
            sources: self.sources.iter().map(|s| s.path.clone()).collect(),
            clips: self
                .clips
                .iter()
                .map(|c| ExportClip {
                    source: c.source,
                    start: c.start,
                    end: c.end,
                    volume: if c.muted { 0.0 } else { c.volume },
                    has_audio: self.sources[c.source]
                        .info
                        .as_ref()
                        .is_some_and(|i| i.has_audio),
                })
                .collect(),
            width,
            height,
            fps: self.export_fps,
            quality: self.quality,
            output,
        };
        self.export = match export::start(job, ctx.clone()) {
            Ok(handle) => ExportState::Running(handle),
            Err(err) => ExportState::Done(Err(format!("{err:#}"))),
        };
    }

    fn export_dialog(&mut self, ctx: &egui::Context) {
        if matches!(self.export, ExportState::Closed) {
            return;
        }
        let p = pal();
        let mut close = false;
        let mut start = false;
        let mut open: Option<(PathBuf, bool)> = None;
        let (w, h) = self.project_size();
        let source_h = self
            .clips
            .first()
            .and_then(|c| self.sources[c.source].info.as_ref())
            .map(|i| i.height)
            .unwrap_or(1080);

        let modal = egui::Modal::new(egui::Id::NULL.with("export-dialog"))
            .frame(
                egui::Frame::new()
                    .fill(p.surface)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::same(20)),
            )
            .show(ctx, |ui| {
                ui.set_width(400.0);
                match &self.export {
                    ExportState::Settings => {
                        section_header(ui, Icon::Export, "Export video", "MP4 · H.264 · AAC");
                        ui.add_space(14.0);
                        field_label(ui, None, "Resolution");
                        let mut options: Vec<(Resolution, Option<Icon>, &str)> =
                            vec![(Resolution::Source, None, "Original")];
                        for (res, label) in [
                            (Resolution::P1080, "1080p"),
                            (Resolution::P720, "720p"),
                            (Resolution::P480, "480p"),
                        ] {
                            if res.height().is_some_and(|rh| rh < source_h) {
                                options.push((res, None, label));
                            }
                        }
                        segmented(ui, "export_res", &mut self.resolution, &options);
                        ui.add_space(10.0);
                        field_label(ui, None, "Frame rate");
                        segmented(
                            ui,
                            "export_fps",
                            &mut self.export_fps,
                            &[
                                (24u32, None, "24 fps"),
                                (30, None, "30 fps"),
                                (60, None, "60 fps"),
                            ],
                        );
                        ui.add_space(10.0);
                        field_label(ui, None, "Quality");
                        segmented(
                            ui,
                            "export_quality",
                            &mut self.quality,
                            &ExportQuality::ALL.map(|q| (q, None, q.label())),
                        );
                        ui.add_space(12.0);
                        info_note(
                            ui,
                            Icon::Info,
                            &format!(
                                "{w}×{h} · {} · {} clips",
                                fmt_time(self.total(), false),
                                self.clips.len()
                            ),
                            p.muted,
                        );
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            let half = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                            if button_sized(ui, None, "Cancel", ButtonKind::Secondary, half, 36.0)
                                .clicked()
                            {
                                close = true;
                            }
                            if button_sized(
                                ui,
                                Some(Icon::Export),
                                "Export…",
                                ButtonKind::Primary,
                                half,
                                36.0,
                            )
                            .clicked()
                            {
                                start = true;
                            }
                        });
                    }
                    ExportState::Running(handle) => {
                        section_header(ui, Icon::Export, "Exporting…", &file_name(&handle.output));
                        ui.add_space(16.0);
                        let fraction = handle.fraction();
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .fill(p.accent)
                                .desired_height(10.0)
                                .corner_radius(CornerRadius::same(4)),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(format!("{:.0}% complete", fraction * 100.0))
                                .size(12.0)
                                .color(p.muted),
                        );
                        ui.add_space(14.0);
                        if wide_button(
                            ui,
                            Some(Icon::Close),
                            "Cancel export",
                            ButtonKind::Secondary,
                            34.0,
                        )
                        .clicked()
                        {
                            handle.cancel();
                        }
                    }
                    ExportState::Done(Ok(path)) => {
                        ui.horizontal(|ui| {
                            icon_badge_sized(ui, Icon::Check, p.success_text, p.success_soft, 36.0);
                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new("Export complete")
                                        .size(15.5)
                                        .family(crate::theme::bold())
                                        .color(p.text),
                                );
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "{} · {}",
                                            file_name(path),
                                            util::human_size(
                                                std::fs::metadata(path)
                                                    .map(|m| m.len())
                                                    .unwrap_or(0)
                                            )
                                        ))
                                        .size(12.0)
                                        .color(p.muted),
                                    )
                                    .truncate(),
                                );
                            });
                        });
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            let third =
                                (ui.available_width() - 2.0 * ui.spacing().item_spacing.x) / 3.0;
                            if button_sized(
                                ui,
                                Some(Icon::Play),
                                "Play",
                                ButtonKind::Primary,
                                third,
                                34.0,
                            )
                            .clicked()
                            {
                                open = Some((path.clone(), false));
                            }
                            if button_sized(
                                ui,
                                Some(Icon::Folder),
                                "Show",
                                ButtonKind::Secondary,
                                third,
                                34.0,
                            )
                            .clicked()
                            {
                                open = Some((path.clone(), true));
                            }
                            if button_sized(ui, None, "Done", ButtonKind::Secondary, third, 34.0)
                                .clicked()
                            {
                                close = true;
                            }
                        });
                    }
                    ExportState::Done(Err(err)) => {
                        ui.horizontal(|ui| {
                            icon_badge_sized(ui, Icon::Alert, p.danger, p.danger_soft, 36.0);
                            ui.label(
                                RichText::new("Export did not finish")
                                    .size(15.5)
                                    .family(crate::theme::bold())
                                    .color(p.text),
                            );
                        });
                        ui.add_space(10.0);
                        ui.add(
                            egui::Label::new(RichText::new(err).size(12.0).color(p.danger_text))
                                .selectable(true)
                                .wrap(),
                        );
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            let half = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                            if button_sized(ui, None, "Close", ButtonKind::Secondary, half, 34.0)
                                .clicked()
                            {
                                close = true;
                            }
                            if button_sized(
                                ui,
                                Some(Icon::Refresh),
                                "Try again",
                                ButtonKind::Primary,
                                half,
                                34.0,
                            )
                            .clicked()
                            {
                                self.export = ExportState::Settings;
                            }
                        });
                    }
                    ExportState::Closed => {}
                }
            });

        if modal.should_close() && !self.is_exporting() {
            close = true;
        }
        if start {
            self.start_export(ctx);
        } else if close {
            self.export = ExportState::Closed;
        }
        if let Some((path, reveal)) = open {
            let result = if reveal {
                crate::shell::reveal_path(&path)
            } else {
                crate::shell::open_path(&path)
            };
            if let Err(err) = result {
                self.notice = Some(format!("{err:#}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn drop_zone(ui: &mut egui::Ui) {
    let p = pal();
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 180.0), Sense::hover());
    let dash = Stroke::new(1.5, p.border_hi);
    let r = rect.shrink(2.0);
    let shapes = egui::Shape::dashed_line(
        &[
            r.left_top(),
            r.right_top(),
            r.right_bottom(),
            r.left_bottom(),
            r.left_top(),
        ],
        dash,
        6.0,
        4.0,
    );
    ui.painter().extend(shapes);
    let center = rect.center();
    icons::paint(
        ui.painter(),
        Rect::from_center_size(center - vec2(0.0, 28.0), Vec2::splat(26.0)),
        Icon::Import,
        p.muted,
    );
    ui.painter().text(
        center + vec2(0.0, 6.0),
        Align2::CENTER_CENTER,
        "Drop videos here",
        FontId::proportional(13.5),
        p.text_soft,
    );
    ui.painter().text(
        center + vec2(0.0, 26.0),
        Align2::CENTER_CENTER,
        "MP4, MOV, MKV, WebM and more",
        FontId::proportional(12.0),
        p.muted,
    );
    ui.add_space(10.0);
    empty_state(
        ui,
        Icon::Record,
        "Or record something",
        "Finished recordings can be opened here from Recent.",
    );
}

/// Column heading shared by Media and Properties.
fn panel_title(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(14.5)
            .family(crate::theme::bold())
            .color(pal().text),
    );
}

fn separator(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, 22.0), Sense::hover());
    ui.painter().line_segment(
        [rect.center_top(), rect.center_bottom()],
        Stroke::new(1.0, pal().border),
    );
}

fn project_row(ui: &mut egui::Ui, label: &str, value: &str) {
    let p = pal();
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(12.0).color(p.muted));
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).size(12.0).color(p.text));
        });
    });
}

fn play_button(ui: &mut egui::Ui, playing: bool) -> egui::Response {
    let p = pal();
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::click());
    let enabled = ui.is_enabled();
    let hovered = response.hovered() && enabled;
    let fill = if !enabled {
        p.toggle_off
    } else if hovered {
        p.accent_hi
    } else {
        p.accent
    };
    ui.painter().circle_filled(rect.center(), 20.0, fill);
    icons::paint(
        ui.painter(),
        Rect::from_center_size(
            rect.center() + vec2(if playing { 0.0 } else { 1.0 }, 0.0),
            Vec2::splat(16.0),
        ),
        if playing { Icon::Pause } else { Icon::Play },
        Color32::WHITE,
    );
    if response.has_focus() {
        ui.painter()
            .circle_stroke(rect.center(), 23.0, Stroke::new(2.0, p.focus));
    }
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response.on_hover_text_if(
        true,
        if playing {
            "Pause (Space)"
        } else {
            "Play (Space)"
        },
    )
}

fn fit_size(image: Vec2, area: Vec2) -> Vec2 {
    let scale = (area.x / image.x.max(1.0)).min(area.y / image.y.max(1.0));
    image * scale
}

fn nearest_thumb(thumbs: &[(f64, TextureHandle)], t: f64) -> &TextureHandle {
    let index = thumbs
        .partition_point(|(time, _)| *time <= t)
        .saturating_sub(1);
    &thumbs[index.min(thumbs.len() - 1)].1
}

/// Position in the clip order where a dragged clip would land.
fn drop_index(starts: &[f64], clips: &[Clip], t: f64, _dragged: usize) -> usize {
    for (i, clip) in clips.iter().enumerate() {
        if t < starts[i] + clip.len() / 2.0 {
            return i;
        }
    }
    clips.len()
}

fn target_insert_time(starts: &[f64], clips: &[Clip], target: usize, _dragged: usize) -> f64 {
    if target < clips.len() {
        starts[target]
    } else {
        starts.last().copied().unwrap_or(0.0) + clips.last().map(Clip::len).unwrap_or(0.0)
    }
}

/// "1:05.40" (with fraction) or "1:05".
pub fn fmt_time(seconds: f64, fraction: bool) -> String {
    let seconds = seconds.max(0.0);
    let whole = seconds.floor() as u64;
    let (h, m, s) = (whole / 3600, (whole % 3600) / 60, whole % 60);
    let base = if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    };
    if fraction {
        format!(
            "{base}.{:02}",
            ((seconds - whole as f64) * 100.0).floor() as u64
        )
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(id: u64, start: f64, end: f64) -> Clip {
        Clip {
            id,
            source: 0,
            start,
            end,
            volume: 1.0,
            muted: false,
        }
    }

    #[test]
    fn formats_times() {
        assert_eq!(fmt_time(65.4, true), "1:05.40");
        assert_eq!(fmt_time(3725.0, false), "1:02:05");
    }

    #[test]
    fn drop_index_uses_clip_midpoints() {
        let clips = vec![clip(1, 0.0, 4.0), clip(2, 0.0, 2.0)];
        let starts = vec![0.0, 4.0];
        assert_eq!(drop_index(&starts, &clips, 1.0, 1), 0);
        assert_eq!(drop_index(&starts, &clips, 4.5, 0), 1);
        assert_eq!(drop_index(&starts, &clips, 9.0, 0), 2);
    }
}
