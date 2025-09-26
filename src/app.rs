use iced::widget::{button, column, container, row, scrollable, slider, text, text_input, Space, svg};
use iced::{Element, Length, Result as IcedResult, Task, Subscription};
use iced::widget::svg::Handle as SvgHandle;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};

// Symphonia is used to probe duration for formats where rodio's Decoder
// cannot determine it up-front (e.g., some MP3/streamable formats).
// This enables the seekbar to work more reliably.
use symphonia::core::formats::FormatOptions as SymFormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions as SymMetadataOptions;
use symphonia::core::probe::Hint as SymHint;
use symphonia::default::get_probe as sym_get_probe;
use symphonia::core::codecs::DecoderOptions as SymDecoderOptions;
use symphonia::default::get_codecs as sym_get_codecs;

pub fn run() -> IcedResult {
    iced::application("Rust Audio Player", update, view)
        .subscription(subscription)
        .theme(app_theme)
        .run()
}

fn app_theme(state: &AudioPlayer) -> iced::Theme {
    if state.dark_mode { iced::Theme::Dark } else { iced::Theme::Light }
}

// Platform-specific async folder picker abstraction
async fn pick_folder_async() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_title("Choose Music Folder")
        .pick_folder()
        .await
        .map(|h| h.path().to_path_buf())
}

#[derive(Debug, Clone)]
enum Message {
    ChooseFolder,
    FolderChosen(Option<PathBuf>),
    SelectTrack(usize),
    TogglePlayPause,
    ToggleTheme,
    Stop,
    NextTrack,
    PrevTrack,
    SearchChanged(String),
    // Seek bar interactions
    SeekChanged(f32),
    SeekReleased,
    // periodic UI refresh
    Tick,
    None,
    // Equalizer
    ToggleEq,
    EqBandChanged(usize, f32),
    EqClose,
}

struct AudioFile {
    name: String,
    path: PathBuf,
}

struct AudioEngine {
    stream: rodio::stream::OutputStream,
    sink: Option<rodio::Sink>,
    now_playing: Option<String>,
    current_path: Option<PathBuf>,
    duration: Option<Duration>,
    start_instant: Option<Instant>,
    paused_at: Option<Duration>,
    position_offset: Duration,
    // Equalizer state
    eq: Arc<Equalizer>, // shared with UI for live updates
}

impl AudioEngine {
    fn new() -> Result<Self, String> {
        // Open the default output stream using the new rodio 0.21 API
        let stream = rodio::OutputStreamBuilder::open_default_stream()
            .map_err(|e| format!("Audio output error: {e}"))?;
        Ok(Self {
            stream,
            sink: None,
            now_playing: None,
            current_path: None,
            duration: None,
            start_instant: None,
            paused_at: None,
            position_offset: Duration::ZERO,
            eq: Arc::new(Equalizer::default()),
        })
    }

    fn stop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
        self.now_playing = None;
        self.current_path = None;
        self.duration = None;
        self.start_instant = None;
        self.paused_at = None;
        self.position_offset = Duration::ZERO;
    }

    fn play_file(&mut self, path: &Path) -> Result<(), String> {
        self.play_from(path, Duration::ZERO, false)
    }

    fn play_from(&mut self, path: &Path, position: Duration, resume_paused: bool) -> Result<(), String> {
        use rodio::Source as _;

        if let Some(sink) = self.sink.take() { sink.stop(); }

        let file = std::fs::File::open(path)
            .map_err(|e| format!("Failed to open file: {e}"))?;
        // Decoder::try_from(File) wraps in BufReader and sets byte_len for accurate seeking
        let decoder = rodio::Decoder::try_from(file)
            .map_err(|e| format!("Failed to decode audio: {e}"))?;

        // Prefer rodio's duration, but if it's not available, try probing with symphonia.
        // Avoid re-probing if we already know duration for the same track.
        let same_track = self.current_path.as_ref().is_some_and(|p| p == path);
        if !same_track || self.duration.is_none() {
            self.duration = decoder.total_duration().or_else(|| probe_duration_with_symphonia(path));
        }

    // Apply EQ by wrapping the source
    let source = decoder.skip_duration(position);
    let source = EqSource::new(source, self.eq.clone());

        // Create a sink we can control and append the (possibly skipped) source
        let sink = rodio::Sink::connect_new(&self.stream.mixer());
    sink.append(source);
        self.sink = Some(sink);
        self.now_playing = Some(
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Unknown")
                .to_string(),
        );
        self.current_path = Some(path.to_path_buf());
        self.position_offset = position;
        self.paused_at = None;
        self.start_instant = Some(Instant::now());

        if resume_paused {
            if let Some(s) = &self.sink { s.pause(); }
        }

        Ok(())
    }

    fn pause(&mut self) {
        if let Some(s) = &self.sink {
            if !s.is_paused() {
                s.pause();
                let pos = self.current_position();
                self.paused_at = Some(pos);
                self.start_instant = None;
            }
        }
    }

    fn resume(&mut self) {
        if let Some(s) = &self.sink {
            if s.is_paused() {
                s.play();
                if let Some(p) = self.paused_at.take() {
                    self.position_offset = p;
                }
                self.start_instant = Some(Instant::now());
            }
        }
    }

    fn seek_to(&mut self, position: Duration) -> Result<(), String> {
        let clamped = if let Some(d) = self.duration { position.min(d) } else { position };
        if let Some(path) = self.current_path.clone() {
            let was_paused = self.sink.as_ref().is_some_and(|s| s.is_paused());
            // If position is close to current, do nothing
            if (self.current_position().as_secs_f32() - clamped.as_secs_f32()).abs() < 0.01 { return Ok(()); }
            self.play_from(&path, clamped, was_paused)
        } else { Ok(()) }
    }

    fn is_playing(&self) -> bool {
        if let Some(s) = &self.sink {
            !s.is_paused() && !s.empty()
        } else {
            false
        }
    }

    fn total_duration(&self) -> Option<Duration> {
        self.duration
    }

    fn current_position(&self) -> Duration {
        if let Some(paused) = self.paused_at {
            paused
        } else if let Some(start) = self.start_instant {
            self.position_offset + start.elapsed()
        } else {
            self.position_offset
        }
    }
}

fn probe_duration_with_symphonia(path: &Path) -> Option<Duration> {
    let mut hint = SymHint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let probed = sym_get_probe()
        .format(&hint, mss, &SymFormatOptions::default(), &SymMetadataOptions::default())
        .ok()?;
    let mut format = probed.format;
    // Choose the default track or the first track with a sample rate.
    let track = format
        .default_track()
        .cloned()
        .or_else(|| format.tracks().iter().find(|t| t.codec_params.sample_rate.is_some()).cloned())?;

    let params = &track.codec_params;
    if let (Some(sr), Some(n_frames)) = (params.sample_rate, params.n_frames) {
        let secs = n_frames as f64 / sr as f64;
        return Some(Duration::from_secs_f64(secs));
    }

    // As a last resort, decode and count frames to compute duration.
    let mut decoder = sym_get_codecs().make(params, &SymDecoderOptions::default()).ok()?;
    let mut total_frames: u64 = 0;
    let mut sr_opt = params.sample_rate;
    let track_id = track.id;

    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id { continue; }
        if let Ok(audio_buf) = decoder.decode(&packet) {
            total_frames += audio_buf.frames() as u64;
            let rate = audio_buf.spec().rate;
            if sr_opt.is_none() { sr_opt = Some(rate); }
        }
    }

    let sr = sr_opt?;
    if total_frames > 0 {
        return Some(Duration::from_secs_f64(total_frames as f64 / sr as f64));
    }

    None
}

struct AudioPlayer {
    folder: Option<PathBuf>,
    files: Vec<AudioFile>,
    selected: Option<usize>,
    audio: Result<AudioEngine, String>,
    status: Option<String>,
    last_click: Option<(usize, Instant)>,
    // Seek bar state
    seek_value: f32,
    is_seeking: bool,
    last_seek_apply: Option<Instant>,
    pre_seek_was_playing: bool,
    // Search/filter state
    search_query: String,
    // Theme state
    dark_mode: bool,
    // EQ UI state and bands (gain in dB)
    eq_visible: bool,
    eq_gains_db: [f32; 10],
}

impl Default for AudioPlayer {
    fn default() -> Self {
        // Start with defaults, then try loading persisted config
        let mut me = Self {
            folder: None,
            files: Vec::new(),
            selected: None,
            audio: AudioEngine::new(),
            status: None,
            last_click: None,
            seek_value: 0.0,
            is_seeking: false,
            last_seek_apply: None,
            pre_seek_was_playing: false,
            search_query: String::new(),
            dark_mode: false,
            eq_visible: false,
            eq_gains_db: [0.0; 10],
        };
        if let Some(cfg) = load_config() {
            me.dark_mode = cfg.dark_mode;
            me.folder = cfg.last_folder;
            if let Some(eq) = cfg.eq {
                me.eq_gains_db = eq;
            }
            if let Ok(engine) = &mut me.audio { engine.eq.set_gains_db(me.eq_gains_db); }
            if let Some(folder) = me.folder.clone() {
                let (files, err) = scan_audio_files(&folder);
                me.files = files;
                me.selected = if me.files.is_empty() { None } else { Some(0) };
                me.status = err;
            }
        }
        me
    }
}

// Update function for iced 0.13 functional API
fn update(state: &mut AudioPlayer, message: Message) -> Task<Message> {
    match message {
        Message::ChooseFolder => {
            // Non-blocking async folder picker
            return Task::perform(pick_folder_async(), Message::FolderChosen);
        }
        Message::FolderChosen(Some(path)) => {
            state.folder = Some(path.clone());
            let (files, errors) = scan_audio_files(&path);
            state.files = files;
            state.selected = if state.files.is_empty() { None } else { Some(0) };
            state.status = errors;
            // Persist last folder
            save_config(&AppConfig { dark_mode: state.dark_mode, last_folder: state.folder.clone(), eq: Some(state.eq_gains_db) });
        }
        Message::FolderChosen(None) => {
            // user canceled
        }
        Message::PrevTrack => {
            // Compute current index before mutable borrow
            let current_idx = current_index(state);
            let filtered = compute_filtered_indices(state);
            // Previous: if we are >3s into the track, restart; else go to previous track.
            match &mut state.audio {
                Ok(engine) => {
                    if let Some(idx) = current_idx {
                        let position = engine.current_position();
                        if position > Duration::from_secs(3) {
                            let _ = engine.seek_to(Duration::ZERO);
                            if engine.is_playing() { state.status = Some("Restarted".into()); }
                        } else if let Some(pos) = filtered.iter().position(|&x| x == idx).and_then(|p| p.checked_sub(1)) {
                            let target_idx = filtered[pos];
                            if let Some(file) = state.files.get(target_idx) {
                                state.selected = Some(target_idx);
                                if let Err(e) = engine.play_file(&file.path) {
                                    state.status = Some(e);
                                } else {
                                    state.status = Some(format!("Playing: {}", file.name));
                                }
                            }
                        }
                    }
                }
                Err(_) => {}
            }
        }
        Message::NextTrack => {
            // Next: advance to next track and play if available.
            let current_idx = current_index(state).or(state.selected);
            let filtered = compute_filtered_indices(state);
            match &mut state.audio {
                Ok(engine) => {
                    if let Some(idx) = current_idx {
                        if let Some(pos) = filtered.iter().position(|&x| x == idx) {
                            if pos + 1 < filtered.len() {
                                let target_idx = filtered[pos + 1];
                                if let Some(file) = state.files.get(target_idx) {
                                    state.selected = Some(target_idx);
                                    if let Err(e) = engine.play_file(&file.path) {
                                        state.status = Some(e);
                                    } else {
                                        state.status = Some(format!("Playing: {}", file.name));
                                    }
                                }
                            }
                        }
                    }
                }
                Err(_) => {}
            }
        }
        Message::SelectTrack(idx) => {
            if idx >= state.files.len() {
                return Task::none();
            }

            let now = Instant::now();
            let is_double = state
                .last_click
                .as_ref()
                .map(|(i, t)| *i == idx && now.duration_since(*t) <= Duration::from_millis(300))
                .unwrap_or(false);

            state.last_click = Some((idx, now));

            // Always set selection on click
            state.selected = Some(idx);

            if is_double {
                // Double click: start playing the clicked item
                if let Ok(engine) = &mut state.audio {
                    if let Some(file) = state.files.get(idx) {
                        if let Err(e) = engine.play_file(&file.path) {
                            state.status = Some(e);
                        } else {
                            state.status = Some(format!("Playing: {}", file.name));
                        }
                    }
                }
            } else {
                // Single click behavior
                if let Ok(engine) = &mut state.audio {
                    if let Some(sink) = &engine.sink {
                        if engine.is_playing() {
                            engine.pause();
                            state.status = Some("Paused".into());
                        } else if sink.is_paused() {
                            engine.resume();
                            state.status = Some("Resumed".into());
                        }
                    }
                }
            }
        }
        Message::TogglePlayPause => {
            match &mut state.audio {
                Ok(engine) => {
                    match &state.selected {
                        Some(idx) if engine.sink.as_ref().map(|s| s.empty()).unwrap_or(true) => {
                            // No active audio in sink -> (re)start selected track
                            if let Some(file) = state.files.get(*idx) {
                                if let Err(e) = engine.play_file(&file.path) {
                                    state.status = Some(e);
                                } else {
                                    state.status = Some(format!("Playing: {}", file.name));
                                }
                            }
                        }
                        _ => {
                            // Toggle pause/resume on existing sink, if any
                            if let Some(s) = &engine.sink {
                                if s.is_paused() {
                                    engine.resume();
                                    state.status = Some("Resumed".into());
                                } else {
                                    engine.pause();
                                    state.status = Some("Paused".into());
                                }
                            } else if let Some(idx) = state.selected {
                                // No sink yet, start playback of selected
                                if let Some(file) = state.files.get(idx) {
                                    if let Err(e) = engine.play_file(&file.path) {
                                        state.status = Some(e);
                                    } else {
                                        state.status = Some(format!("Playing: {}", file.name));
                                    }
                                }
                            } else {
                                state.status = Some("No track selected.".into());
                            }
                        }
                    }
                }
                Err(e) => {
                    state.status = Some(format!(
                        "Audio not initialized: {e}. Try restarting the app."
                    ));
                }
            }
        }
        Message::Stop => {
            if let Ok(engine) = &mut state.audio {
                engine.stop();
            }
            state.status = Some("Stopped.".into());
        }
        Message::ToggleTheme => {
            state.dark_mode = !state.dark_mode;
            save_config(&AppConfig { dark_mode: state.dark_mode, last_folder: state.folder.clone(), eq: Some(state.eq_gains_db) });
        }
        Message::ToggleEq => {
            state.eq_visible = !state.eq_visible;
        }
        Message::EqBandChanged(idx, val) => {
            // val is slider 0..1 => map to gain range -12..+12 dB, center 0.5 -> 0 dB
            if idx < state.eq_gains_db.len() {
                let gain_db = (val - 0.5) * 24.0;
                state.eq_gains_db[idx] = gain_db;
                // Update engine's EQ immediately; restart current playback at same position to apply
                if let Ok(engine) = &mut state.audio { engine.eq.set_gains_db(state.eq_gains_db); }
                save_config(&AppConfig { dark_mode: state.dark_mode, last_folder: state.folder.clone(), eq: Some(state.eq_gains_db) });
            }
        }
        Message::EqClose => { state.eq_visible = false; }
        Message::SearchChanged(q) => {
            state.search_query = q;
            // Optionally, maintain selection if still visible. If not visible, keep it unchanged.
        }
        Message::SeekChanged(value) => {
            // Update the slider visually; don't perform heavy seeks while dragging.
            let was_seeking = state.is_seeking;
            state.seek_value = value.clamp(0.0, 1.0);
            if !was_seeking {
                if let Ok(engine) = &mut state.audio {
                    // Remember whether we were playing and pause during drag for responsiveness.
                    state.pre_seek_was_playing = engine.is_playing();
                    engine.pause();
                }
            }
            state.is_seeking = true;
        }
        Message::SeekReleased => {
            // Apply a single seek when the user releases the slider, then resume if needed.
            if let Ok(engine) = &mut state.audio {
                if let Some(total) = engine.total_duration() {
                    let position = Duration::from_secs_f32(total.as_secs_f32() * state.seek_value);
                    match engine.seek_to(position) {
                        Ok(()) => {
                            if state.pre_seek_was_playing {
                                engine.resume();
                            }
                        }
                        Err(e) => state.status = Some(e),
                    }
                }
            }
            state.is_seeking = false;
            state.last_seek_apply = None;
            state.pre_seek_was_playing = false;
        }
        Message::Tick => {
            // Auto-advance when the current sink finishes.
            let current_idx = current_index(state).or(state.selected);
            let filtered = compute_filtered_indices(state);
            if let Ok(engine) = &mut state.audio {
                if let Some(sink) = &engine.sink {
                    // If playing and became empty => advance
                    if !sink.is_paused() && sink.empty() {
                        if let Some(idx) = current_idx {
                            if let Some(pos) = filtered.iter().position(|&x| x == idx) {
                                if pos + 1 < filtered.len() {
                                    let target_idx = filtered[pos + 1];
                                    if let Some(file) = state.files.get(target_idx) {
                                        state.selected = Some(target_idx);
                                        if let Err(e) = engine.play_file(&file.path) {
                                            state.status = Some(e);
                                        } else {
                                            state.status = Some(format!("Playing: {}", file.name));
                                        }
                                    }
                                } else {
                                    // Reached the end, stop and clear.
                                    engine.stop();
                                    state.status = Some("Playback finished.".into());
                                }
                            }
                        }
                    }
                }
            }
        }
        Message::None => {}
    }
    // No background task; return none. The UI will refresh on interactions.
    Task::none()
}

fn subscription(_state: &AudioPlayer) -> Subscription<Message> {
    // Refresh UI at ~10 FPS so the progress/time update while playing
    iced::time::every(Duration::from_millis(100)).map(|_| Message::Tick)
}

fn view(state: &AudioPlayer) -> Element<'_, Message> {

    // Search bar
    let search_bar = row![
        text_input("Search songs...", &state.search_query)
            .on_input(Message::SearchChanged)
            .padding(8)
            .width(Length::Fill),
        Space::with_width(Length::Fixed(8.0)),
        button("Clear").on_press(Message::SearchChanged(String::new()))
    ]
    .spacing(8)
    .width(Length::Fill);

    // Files list (filtered)
    let mut files_col = column![];
    let playing_idx = current_index(state);
    let (is_playing, is_paused) = match &state.audio {
        Ok(engine) => {
            let paused = engine.sink.as_ref().is_some_and(|s| s.is_paused());
            (engine.is_playing(), paused)
        }
        Err(_) => (false, false),
    };
    let filtered = compute_filtered_indices(state);
    for &i in filtered.iter() {
        let file = &state.files[i];
        let selected = state.selected == Some(i);
        // Show plain label; selection will be indicated via background color
        let mut label = file.name.clone();
        if Some(i) == playing_idx {
            if is_paused {
                label = format!("[PAUSED] {}", label);
            } else if is_playing {
                label = format!("[PLAYING] {}", label);
            }
        }
        files_col = files_col.push(
            button(text(label))
                .on_press(Message::SelectTrack(i))
                .width(Length::Fill)
                .padding([6, 10])
                .style(move |theme, status| {
                    use iced::widget::button;
                    if selected {
                        // Keep the primary (blue) style but make it visibly darker for selection
                        let mut style = button::primary(theme, status);
                        let palette = theme.extended_palette();
                        let mut c = palette.primary.strong.color;
                        // Darken the current primary color
                        let f: f32 = 0.80;
                        c.r *= f;
                        c.g *= f;
                        c.b *= f;
                        style.background = Some(iced::Background::from(c));
                        style.text_color = palette.primary.strong.text;
                        style
                    } else {
                        // Regular primary blue for unselected items
                        button::primary(theme, status)
                    }
                }),
        );
    }
    let files_list = scrollable(files_col.spacing(4).width(Length::Fill))
        .height(Length::Fill)
        .width(Length::Fill);

    let is_playing_now = match &state.audio { Ok(e) => e.is_playing(), Err(_) => false };
    // Determine availability of prev/next based on current selection
    let curr_idx = current_index(state).or(state.selected);
    let filtered = compute_filtered_indices(state);
    let (can_prev, can_next) = if let Some(ci) = curr_idx {
        if let Some(pos) = filtered.iter().position(|&x| x == ci) {
            (pos > 0, pos + 1 < filtered.len())
        } else {
            (false, false)
        }
    } else {
        (false, false)
    };

    // Helper to make a round icon button with an SVG
    fn round_icon_button<'a, M: Clone + 'a>(svg_bytes: &'static [u8], on_press: Option<M>) -> iced::widget::Button<'a, M> {
        let handle = SvgHandle::from_memory(svg_bytes);
        let svg_img = svg(handle).width(Length::Fixed(32.0)).height(Length::Fixed(32.0));
        let content = container(svg_img)
            .width(Length::Fixed(44.0))
            .height(Length::Fixed(44.0))
            .center_x(Length::Fixed(44.0))
            .center_y(Length::Fixed(44.0));
        let mut b = button(content)
            .padding(0)
            .style(|theme, status| {
                use iced::widget::button;
                let mut s = button::secondary(theme, status);
                s.border.radius = 22.0.into();
                s
            });
        if let Some(msg) = on_press { b = b.on_press(msg); }
        b
    }

    // Embed SVGs at compile-time for portability
    static PLAY_SVG: &[u8] = include_bytes!("../assets/play.svg");
    static PAUSE_SVG: &[u8] = include_bytes!("../assets/pause.svg");
    static STOP_SVG: &[u8] = include_bytes!("../assets/stop.svg");
    static PREV_SVG: &[u8] = include_bytes!("../assets/prev.svg");
    static NEXT_SVG: &[u8] = include_bytes!("../assets/next.svg");
    static SUN_SVG: &[u8] = include_bytes!("../assets/sun.svg");
    static MOON_SVG: &[u8] = include_bytes!("../assets/moon.svg");
    static EQ_SVG: &[u8] = include_bytes!("../assets/eq.svg");

    // Theme toggle button: show opposite of current theme
    let theme_btn = round_icon_button(if state.dark_mode { SUN_SVG } else { MOON_SVG }, Some(Message::ToggleTheme));

    let mut prev_btn = round_icon_button(PREV_SVG, None);
    if can_prev { prev_btn = round_icon_button(PREV_SVG, Some(Message::PrevTrack)); }

    let play_btn = round_icon_button(if is_playing_now { PAUSE_SVG } else { PLAY_SVG }, Some(Message::TogglePlayPause));

    let mut next_btn = round_icon_button(NEXT_SVG, None);
    if can_next { next_btn = round_icon_button(NEXT_SVG, Some(Message::NextTrack)); }

    let stop_btn = round_icon_button(STOP_SVG, Some(Message::Stop));

    let controls = row![
        Space::with_width(Length::Fill),
        prev_btn,
        Space::with_width(Length::Fixed(12.0)),
        play_btn,
        Space::with_width(Length::Fixed(12.0)),
        next_btn,
        Space::with_width(Length::Fixed(20.0)),
        stop_btn,
        Space::with_width(Length::Fill),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center)
    .width(Length::Fill);

    let eq_btn = round_icon_button(EQ_SVG, Some(Message::ToggleEq));
    let header = row![
        text("Rust Audio Player").size(22),
        Space::with_width(Length::FillPortion(1)),
        theme_btn,
        Space::with_width(Length::Fixed(8.0)),
        eq_btn,
        Space::with_width(Length::Fixed(8.0)),
        button("Choose Folder").on_press(Message::ChooseFolder),
        Space::with_width(Length::Fixed(12.0)),
        text(state.folder_display()).size(16)
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center)
    .width(Length::Fill);

    // Build progress/seek UI
    let (slider_enabled, slider_value, time_text) = match &state.audio {
        Ok(engine) => {
            if let Some(total) = engine.total_duration() {
                let total_secs = total.as_secs_f32().max(0.001);
                let ratio = (engine.current_position().as_secs_f32() / total_secs).clamp(0.0, 1.0);
                let value = if state.is_seeking { state.seek_value } else { ratio };
                (true, value, format!("{} / {}", format_time(engine.current_position()), format_time(total)))
            } else {
                (false, 0.0, String::new())
            }
        }
        Err(_) => (false, 0.0, String::new()),
    };

    let seek_bar = if slider_enabled {
        slider(0.0..=1.0, slider_value, Message::SeekChanged)
            .step(0.001)
            .on_release(Message::SeekReleased)
            .width(Length::Fill)
    } else {
        slider(0.0..=1.0, 0.0, |_| Message::None).width(Length::Fill)
    };

    let progress_row = row![seek_bar, Space::with_width(Length::Fixed(8.0)), text(time_text)]
        .spacing(8)
        .width(Length::Fill);

    let status_line = {
        let audio_line = match &state.audio {
            Ok(engine) => {
                if let Some(np) = &engine.now_playing {
                    if engine.sink.as_ref().is_some_and(|s| s.is_paused()) {
                        format!("Paused: {}", np)
                    } else {
                        format!("Now playing: {}", np)
                    }
                } else {
                    "Idle".into()
                }
            }
            Err(e) => format!("Audio init error: {e}"),
        };
        let extra = state.status.as_deref().unwrap_or("");
        let combined = if extra.is_empty() {
            audio_line
        } else {
            format!("{audio_line} — {extra}")
        };
        text(combined)
    };

    // Optional EQ popup panel
    let eq_popup = if state.eq_visible {
        let bands_hz: [f32; 10] = [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
        let mut sliders = row![];
        for (i, f) in bands_hz.iter().enumerate() {
            // Map db -12..+12 to slider 0..1
            let v = (state.eq_gains_db[i] / 24.0) + 0.5;
            let v = v.clamp(0.0, 1.0);
            let s = column![
                text(format!("{:.0} Hz", f)).size(12),
                // Use horizontal slider but stack vertically; keep compact width
                slider(0.0..=1.0, v, move |x| Message::EqBandChanged(i, x))
                    .step(0.01)
                    .width(Length::Fixed(140.0)),
                text(format!("{:+.1} dB", state.eq_gains_db[i])).size(12),
            ]
            .spacing(6)
            .width(Length::Fixed(160.0));
            sliders = sliders.push(s);
        }
        Some(container(
            column![
                row![text("Equalizer").size(18), Space::with_width(Length::Fill), button("Close").on_press(Message::EqClose)],
                Space::with_height(8),
                sliders.spacing(10)
            ]
            .spacing(8)
            .padding(8)
        )
        .width(Length::Fill))
    } else { None };

    let content_col = column![
        header,
        Space::with_height(8),
        controls,
        Space::with_height(8),
        progress_row,
        Space::with_height(8),
    if let Some(eq) = eq_popup { eq } else { container(Space::with_height(0)).into() },
        Space::with_height(8),
        search_bar,
        Space::with_height(8),
        container(files_list)
            .height(Length::Fill)
            .width(Length::Fill)
            .padding(4),
        Space::with_height(8),
        status_line
    ]
    .padding(16)
    .spacing(10)
    .height(Length::Fill);

    container(content_col)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn format_time(dur: Duration) -> String {
    let secs = dur.as_secs();
    let m = secs / 60;
    let s = secs % 60;
    format!("{:02}:{:02}", m, s)
}

impl AudioPlayer {
    fn folder_display(&self) -> String {
        self.folder
            .as_ref()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "No folder selected".into())
    }
}

// Helper: determine the current track index, preferring the engine's current_path if available.
fn current_index(state: &AudioPlayer) -> Option<usize> {
    if let Ok(engine) = &state.audio {
        if let Some(p) = &engine.current_path {
            return state.files.iter().position(|f| &f.path == p).or(state.selected);
        }
    }
    state.selected
}

// Compute the indices of files that match the current search query (case-insensitive substring)
fn compute_filtered_indices(state: &AudioPlayer) -> Vec<usize> {
    if state.search_query.trim().is_empty() {
        return (0..state.files.len()).collect();
    }
    let q = state.search_query.to_lowercase();
    state
        .files
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let name = f.name.to_lowercase();
            if name.contains(&q) { Some(i) } else { None }
        })
        .collect()
}

fn scan_audio_files(dir: &Path) -> (Vec<AudioFile>, Option<String>) {
    // Filter by common audio extensions. With rodio + symphonia-all, this should cover most use cases.
    const EXTS: &[&str] = &[
        "mp3", "flac", "wav", "ogg", "opus", "aac", "m4a", "alac", "aiff", "aif",
    ];

    let mut files = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    match fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(e) => {
                        let path = e.path();
                        if path.is_file() {
                            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                                if EXTS.iter().any(|x| x.eq_ignore_ascii_case(ext)) {
                                    let name = path
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("Unknown")
                                        .to_string();
                                    files.push(AudioFile { name, path });
                                }
                            }
                        }
                    }
                    Err(e) => errors.push(format!("Error reading entry: {e}")),
                }
            }
        }
        Err(e) => errors.push(format!("Failed to read directory: {e}")),
    }

    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    let err = if errors.is_empty() {
        None
    } else {
        Some(errors.join("; "))
    };
    (files, err)
}

// --- Tiny config (theme + last folder) ---
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct AppConfig {
    dark_mode: bool,
    #[serde(with = "opt_path")] // serialize Option<PathBuf> as string path
    last_folder: Option<PathBuf>,
    // Equalizer gains
    eq: Option<[f32; 10]>,
}

fn config_path() -> Option<PathBuf> {
    use directories::ProjectDirs;
    let proj = ProjectDirs::from("dev", "RustSamples", "RustAudioPlayer")?;
    let dir = proj.config_dir();
    std::fs::create_dir_all(dir).ok()?;
    Some(dir.join("settings.json"))
}

fn load_config() -> Option<AppConfig> {
    let path = config_path()?;
    let data = std::fs::read_to_string(path).ok()?;
    let mut cfg: AppConfig = serde_json::from_str(&data).ok()?;
    // Validate last folder exists
    if let Some(ref p) = cfg.last_folder {
        if !p.exists() {
            cfg.last_folder = None;
        }
    }
    Some(cfg)
}

fn save_config(cfg: &AppConfig) {
    if let Some(path) = config_path() {
        if let Ok(json) = serde_json::to_string_pretty(cfg) {
            let _ = std::fs::write(path, json);
        }
    }
}

// serde helpers for Option<PathBuf> as plain string
mod opt_path {
    use super::*;
    use serde::{Serializer, Deserializer};

    pub fn serialize<S>(val: &Option<PathBuf>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match val {
            Some(p) => s.serialize_some(&p.to_string_lossy()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(d: D) -> Result<Option<PathBuf>, D::Error>
    where
        D: Deserializer<'de>,
    {
    let opt: Option<String> = <Option<String> as serde::Deserialize>::deserialize(d)?;
        Ok(opt.map(PathBuf::from))
    }
}

// ===== Equalizer implementation =====
#[derive(Clone, Copy)]
struct BiquadCoeffs { b0: f32, b1: f32, b2: f32, a1: f32, a2: f32 }

#[derive(Clone, Copy, Default)]
struct BiquadState { z1: f32, z2: f32 }

impl BiquadState {
    fn process(&mut self, x: f32, c: BiquadCoeffs) -> f32 {
        // Direct Form I transposed
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }
}

fn peaking_eq(sr: f32, f0: f32, q: f32, gain_db: f32) -> BiquadCoeffs {
    let a = 10f32.powf(gain_db / 40.0);
    let w0 = 2.0 * std::f32::consts::PI * (f0 / sr);
    let alpha = w0.sin() / (2.0 * q);
    let cosw = w0.cos();

    let b0 = 1.0 + alpha * a;
    let b1 = -2.0 * cosw;
    let b2 = 1.0 - alpha * a;
    let a0 = 1.0 + alpha / a;
    let a1 = -2.0 * cosw;
    let a2 = 1.0 - alpha / a;

    let inv_a0 = 1.0 / a0;
    BiquadCoeffs { b0: b0 * inv_a0, b1: b1 * inv_a0, b2: b2 * inv_a0, a1: a1 * inv_a0, a2: a2 * inv_a0 }
}

struct Equalizer {
    gains_db: Mutex<[f32; 10]>,
    version: AtomicU64,
}
impl Default for Equalizer { fn default() -> Self { Self { gains_db: Mutex::new([0.0; 10]), version: AtomicU64::new(0) } } }
impl Equalizer {
    fn set_gains_db(&self, gains: [f32; 10]) {
        if let Ok(mut g) = self.gains_db.lock() { *g = gains; }
        self.version.fetch_add(1, Ordering::Relaxed);
    }
    fn snapshot_gains(&self) -> [f32; 10] {
        self.gains_db.lock().map(|g| *g).unwrap_or([0.0; 10])
    }
}

struct EqSource<S: rodio::Source> {
    inner: S,
    // Per-band coefficients at current sample rate
    coeffs: [BiquadCoeffs; 10],
    // Stereo states for each band
    l: [BiquadState; 10],
    r: [BiquadState; 10],
    next_left: bool,
    shared: Arc<Equalizer>,
    last_version: u64,
    // Small fade-in to mask discontinuity on (re)start
    fade_len: u32,
    fade_idx: u32,
}

impl<S: rodio::Source> EqSource<S> {
    fn new(inner: S, shared: Arc<Equalizer>) -> Self {
        let sr = inner.sample_rate() as f32;
        let freqs = [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
        let q = 1.0; // broad bands
        let mut coeffs = [BiquadCoeffs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 }; 10];
        let gains = shared.snapshot_gains();
        for i in 0..10 { coeffs[i] = peaking_eq(sr, freqs[i], q, gains[i]); }
        let last_version = shared.version.load(Ordering::Relaxed);
        let fade_len = ((sr * 0.005).ceil() as u32).max(1);
        Self { inner, coeffs, l: [BiquadState::default(); 10], r: [BiquadState::default(); 10], next_left: true, shared, last_version, fade_len, fade_idx: 0 }
    }
}

impl<S: rodio::Source<Item = f32>> Iterator for EqSource<S> {
    type Item = f32;
    fn next(&mut self) -> Option<Self::Item> {
        let mut x = self.inner.next()?;
        // Refresh coeffs if updated
        let current_version = self.shared.version.load(Ordering::Relaxed);
        if current_version != self.last_version {
            let sr = self.inner.sample_rate() as f32;
            let freqs = [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
            let q = 1.0;
            let gains = self.shared.snapshot_gains();
            for i in 0..10 { self.coeffs[i] = peaking_eq(sr, freqs[i], q, gains[i]); }
            self.last_version = current_version;
        }
        if self.next_left {
            for i in 0..10 { x = self.l[i].process(x, self.coeffs[i]); }
        } else {
            for i in 0..10 { x = self.r[i].process(x, self.coeffs[i]); }
        }
        // Apply fade-in ramp
        if self.fade_idx < self.fade_len {
            let t = self.fade_idx as f32 / self.fade_len as f32;
            x *= t;
            self.fade_idx += 1;
        }
        self.next_left = !self.next_left;
        Some(x)
    }
}

impl<S: rodio::Source<Item = f32>> rodio::Source for EqSource<S> {
    fn channels(&self) -> u16 { self.inner.channels() }
    fn sample_rate(&self) -> u32 { self.inner.sample_rate() }
    fn current_span_len(&self) -> Option<usize> { self.inner.current_span_len() }
    fn total_duration(&self) -> Option<Duration> { self.inner.total_duration() }
}
