use crate::preview::{self, Content, Preview};
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

pub fn show(path: PathBuf) -> Result<(), String> {
    let settings = crate::settings::Settings::load()?;
    #[cfg(windows)]
    let navigator = crate::windows::ExplorerNavigator::from_environment()?;
    #[cfg(windows)]
    let activate = navigator.is_none();
    #[cfg(not(windows))]
    let activate = true;
    let title = format!(
        "Hop - {}",
        path.file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
    );
    let options = eframe::NativeOptions {
        viewport: preview_viewport(activate),
        ..Default::default()
    };
    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            #[cfg(windows)]
            let outside_clicks = Some(crate::windows::OutsideClickMonitor::new(cc)?);
            let app = PreviewApp::new(path, settings, &cc.egui_ctx);
            #[cfg(windows)]
            let app = PreviewApp {
                navigator,
                outside_clicks,
                ..app
            };
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| format!("Cannot open preview window: {error}"))
}

fn preview_viewport(activate: bool) -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_inner_size([900.0, 640.0])
        .with_min_inner_size([400.0, 280.0])
        .with_window_level(egui::WindowLevel::AlwaysOnTop)
        .with_active(activate)
}

pub fn show_settings() -> Result<(), String> {
    let mut pending = crate::settings::Settings::load();
    if let Err(error) = &pending {
        eprintln!("Hop: {error}");
    }
    let mut save_error: Option<String> = None;
    eframe::run_simple_native(
        "Hop - Settings",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([440.0, 240.0])
                .with_min_inner_size([400.0, 240.0]),
            ..Default::default()
        },
        move |ctx, _frame| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("Settings");
                ui.add_space(12.0);
                match &mut pending {
                    Ok(settings) => {
                        ui.add(
                            egui::Slider::new(&mut settings.default_volume, 0.0..=100.0)
                                .text("Default video volume")
                                .suffix("%")
                                .integer(),
                        );
                        ui.add(
                            egui::Slider::new(&mut settings.controls_hide_delay, 0.5..=30.0)
                                .text("Controls auto-hide delay")
                                .suffix(" s")
                                .step_by(0.5),
                        );
                        ui.add_space(8.0);
                        ui.label("Changes apply when you reopen the preview.");
                        if let Some(error) = &save_error {
                            ui.colored_label(ui.visuals().error_fg_color, error);
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                match settings.save() {
                                    Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                                    Err(error) => {
                                        eprintln!("Hop: {error}");
                                        save_error = Some(error);
                                    }
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            if ui.button("Restore defaults").clicked() {
                                *settings = crate::settings::Settings::default();
                                save_error = None;
                            }
                        });
                    }
                    Err(error) => {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                        if ui.button("Reset to defaults").clicked() {
                            pending = Ok(crate::settings::Settings::default());
                        }
                    }
                }
            });
        },
    )
    .map_err(|error| format!("Cannot open settings: {error}"))
}

pub fn show_about() -> Result<(), String> {
    eframe::run_simple_native(
        "Hop - About",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([360.0, 200.0])
                .with_resizable(false),
            ..Default::default()
        },
        |ctx, _frame| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("Hop");
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.add_space(12.0);
                ui.label("Quick file previews for Windows and Linux.");
                ui.label("Images, GIFs, video, text, folders, and file metadata.");
                ui.add_space(12.0);
                if ui.button("Close").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        },
    )
    .map_err(|error| format!("Cannot open About: {error}"))
}

struct PreviewApp {
    settings: crate::settings::Settings,
    path: PathBuf,
    requests: Sender<PathBuf>,
    receiver: Receiver<(PathBuf, Result<Preview, String>)>,
    loading: bool,
    preview: Option<Preview>,
    error: Option<String>,
    texture: Option<egui::TextureHandle>,
    close_keys_released: bool,
    animation_started: Option<Instant>,
    animation_frame: Option<usize>,
    video: Option<crate::media::VideoPlayer>,
    seek_position: Option<f64>,
    control_error: Option<String>,
    controls_last_activity: Option<f64>,
    #[cfg(windows)]
    outside_clicks: Option<crate::windows::OutsideClickMonitor>,
    #[cfg(not(windows))]
    has_had_focus: bool,
    #[cfg(windows)]
    navigator: Option<crate::windows::ExplorerNavigator>,
    #[cfg(windows)]
    navigation_error: Option<String>,
}

fn close_requested(input: &egui::InputState, armed: &mut bool) -> bool {
    if !input.key_down(egui::Key::Space) && !input.key_down(egui::Key::Escape) {
        *armed = true;
    }
    *armed
        && input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Space | egui::Key::Escape,
                    pressed: true,
                    repeat: false,
                    ..
                }
            )
        })
}

fn handle_close_keys(ctx: &egui::Context, armed: &mut bool) {
    let close = ctx.input(|input| close_requested(input, armed));
    if close {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

#[cfg(any(not(windows), test))]
fn close_on_focus_loss(focused: Option<bool>, has_had_focus: &mut bool) -> bool {
    match focused {
        Some(true) => {
            *has_had_focus = true;
            false
        }
        Some(false) => *has_had_focus,
        None => false,
    }
}

#[cfg(any(windows, test))]
fn take_navigation_keys(ctx: &egui::Context) -> Vec<egui::Key> {
    let mut keys = Vec::new();
    ctx.input_mut(|input| {
        input.events.retain(|event| {
            if let egui::Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } = event
                && modifiers.is_none()
                && matches!(
                    key,
                    egui::Key::ArrowLeft
                        | egui::Key::ArrowRight
                        | egui::Key::ArrowUp
                        | egui::Key::ArrowDown
                )
            {
                if *pressed {
                    keys.push(*key);
                }
                return false;
            }
            true
        });
    });
    keys
}

impl PreviewApp {
    fn new(path: PathBuf, settings: crate::settings::Settings, ctx: &egui::Context) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let (requests, pending) = mpsc::channel::<PathBuf>();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            while let Ok(next) = pending.recv() {
                // Serialize decoding and skip intermediate requests during rapid navigation.
                let path = pending.try_iter().last().unwrap_or(next);
                let result = preview::load(&path);
                if sender.send((path, result)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        let mut app = Self {
            settings,
            path: path.clone(),
            requests,
            receiver,
            loading: true,
            preview: None,
            error: None,
            texture: None,
            close_keys_released: false,
            animation_started: None,
            animation_frame: None,
            video: None,
            seek_position: None,
            control_error: None,
            controls_last_activity: None,
            #[cfg(windows)]
            outside_clicks: None,
            #[cfg(not(windows))]
            has_had_focus: false,
            #[cfg(windows)]
            navigator: None,
            #[cfg(windows)]
            navigation_error: None,
        };
        app.request_path(path);
        app
    }

    fn request_path(&mut self, path: PathBuf) {
        self.path = path;
        self.preview = None;
        self.error = None;
        self.texture = None;
        self.animation_started = None;
        self.animation_frame = None;
        self.video = None;
        self.seek_position = None;
        self.control_error = None;
        self.controls_last_activity = None;
        self.loading = true;
        if let Err(error) = self.requests.send(self.path.clone()) {
            let error = format!("Cannot request file preview: {error}");
            eprintln!("Hop: {error}");
            self.error = Some(error);
            self.loading = false;
        }
    }

    #[cfg(any(windows, test))]
    fn load_path(&mut self, path: PathBuf, ctx: &egui::Context) {
        self.request_path(path);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "Hop - {}",
            self.path
                .file_name()
                .unwrap_or(self.path.as_os_str())
                .to_string_lossy()
        )));
        ctx.request_repaint();
    }

    #[cfg(windows)]
    fn handle_navigation(&mut self, ctx: &egui::Context) {
        let Some(mut navigator) = self.navigator.take() else {
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(16));
        let keys = take_navigation_keys(ctx);
        let result = (|| {
            if navigator.close_requested() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            for key in keys {
                let direction = match key {
                    egui::Key::ArrowLeft => crate::windows::Direction::Left,
                    egui::Key::ArrowRight => crate::windows::Direction::Right,
                    egui::Key::ArrowUp => crate::windows::Direction::Up,
                    egui::Key::ArrowDown => crate::windows::Direction::Down,
                    _ => unreachable!("Only arrow keys are collected"),
                };
                navigator.navigate(direction)?;
            }
            navigator.poll()
        })();
        match result {
            Ok(path) => {
                self.navigation_error = None;
                if let Some(path) = path
                    && path != self.path
                {
                    self.load_path(path, ctx);
                }
                self.navigator = Some(navigator);
            }
            Err(error) => {
                if self.navigation_error.as_deref() != Some(error.as_str()) {
                    eprintln!("Hop: {error}");
                }
                if navigator.can_retry() {
                    self.navigator = Some(navigator);
                }
                self.navigation_error = Some(error);
            }
        }
    }

    fn receive(&mut self, ctx: &egui::Context) {
        loop {
            match self.receiver.try_recv() {
                Ok((path, result)) => self.accept_result(path, result, ctx),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.loading {
                        self.accept_result(
                            self.path.clone(),
                            Err("Preview worker stopped unexpectedly.".into()),
                            ctx,
                        );
                    }
                    break;
                }
            }
        }
        if self.loading {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn accept_result(
        &mut self,
        path: PathBuf,
        result: Result<Preview, String>,
        ctx: &egui::Context,
    ) {
        if path != self.path {
            return;
        }
        self.loading = false;
        match result {
            Ok(mut preview) => {
                if matches!(
                    preview.content,
                    Content::Text {
                        truncated: true,
                        ..
                    } | Content::Directory {
                        truncated: true,
                        ..
                    }
                ) {
                    eprintln!(
                        "Hop: {}: preview truncated to its display limit.",
                        self.path.display()
                    );
                }
                if let Content::Image { pixels, size } = &mut preview.content {
                    self.texture = Some(ctx.load_texture(
                        "file-preview",
                        egui::ColorImage::new(*size, std::mem::take(pixels)),
                        egui::TextureOptions::LINEAR,
                    ));
                }
                if let Content::Unsupported {
                    icon: Some(icon), ..
                } = &preview.content
                {
                    self.texture = Some(ctx.load_texture(
                        "file-type-icon",
                        egui::ColorImage::from_rgba_unmultiplied(icon.size, &icon.rgba),
                        egui::TextureOptions::LINEAR,
                    ));
                }
                match &preview.content {
                    Content::Animation(_) => self.animation_started = Some(Instant::now()),
                    Content::Video => match crate::media::VideoPlayer::open(
                        &self.path,
                        self.settings.default_volume,
                    ) {
                        Ok(player) => self.video = Some(player),
                        Err(error) => {
                            eprintln!("Hop: {}: {error}", self.path.display());
                            self.error = Some(error);
                        }
                    },
                    _ => {}
                }
                self.preview = Some(preview);
            }
            Err(error) => {
                eprintln!("Hop: {}: {error}", self.path.display());
                self.error = Some(error);
            }
        }
    }

    fn update_media(&mut self, ctx: &egui::Context) {
        if let Some(started) = self.animation_started
            && let Some(Preview {
                content: Content::Animation(frames),
            }) = &self.preview
        {
            let (index, remaining) = animation_at(frames, started.elapsed());
            if self.animation_frame != Some(index) {
                let frame = &frames[index];
                let image = egui::ColorImage::from_rgba_unmultiplied(frame.size, &frame.rgba);
                update_texture(&mut self.texture, ctx, image);
                self.animation_frame = Some(index);
            }
            ctx.request_repaint_after(remaining);
        }
        if let Some(video) = &mut self.video {
            let size = ctx.available_rect().size();
            match video.render([size.x.max(1.0) as usize, size.y.max(1.0) as usize]) {
                Ok(Some(image)) => update_texture(&mut self.texture, ctx, image),
                Ok(None) => {}
                Err(error) => {
                    eprintln!("Hop: {}: {error}", self.path.display());
                    self.error = Some(error);
                    self.video = None;
                }
            }
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    fn show_video_controls(&mut self, ctx: &egui::Context) {
        let Some(video) = &mut self.video else {
            return;
        };
        if !video_controls_visible(
            ctx,
            &mut self.controls_last_activity,
            self.settings.controls_hide_delay,
        ) {
            return;
        }
        let state = match video.state() {
            Ok(state) => state,
            Err(error) => {
                if self.control_error.as_ref() != Some(&error) {
                    eprintln!("Hop: {error}");
                }
                self.control_error = Some(error);
                return;
            }
        };
        let commands = video_controls_overlay(ctx, state, &mut self.seek_position);
        for command in commands {
            match video.control(command) {
                Ok(()) => self.control_error = None,
                Err(error) => {
                    eprintln!("Hop: {error}");
                    self.control_error = Some(error);
                }
            }
        }
    }
}

fn video_controls_visible(
    ctx: &egui::Context,
    last_activity: &mut Option<f64>,
    hide_delay: f64,
) -> bool {
    let (now, active) = ctx.input(|input| {
        let active = input.pointer.any_down()
            || input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::PointerMoved(_)
                        | egui::Event::PointerButton { .. }
                        | egui::Event::MouseWheel { .. }
                        | egui::Event::Key { pressed: true, .. }
                )
            });
        (input.time, active)
    });
    let last_activity = last_activity.get_or_insert(now);
    if active {
        *last_activity = now;
    }
    let remaining = hide_delay - (now - *last_activity);
    if remaining > 0.0 {
        ctx.request_repaint_after(Duration::from_secs_f64(remaining));
        true
    } else {
        false
    }
}

fn video_controls_overlay(
    ctx: &egui::Context,
    state: crate::media::PlaybackState,
    seek_position: &mut Option<f64>,
) -> Vec<crate::media::PlaybackCommand> {
    egui::Area::new(egui::Id::new("video-controls"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -12.0))
        .order(egui::Order::Foreground)
        .movable(false)
        .fade_in(false)
        .show(ctx, |ui| {
            *ui.visuals_mut() = egui::Visuals::dark();
            egui::Frame::new()
                .fill(egui::Color32::from_black_alpha(210))
                .stroke(egui::Stroke::NONE)
                .corner_radius(10)
                .inner_margin(12)
                .show(ui, |ui| {
                    ui.set_width((ctx.content_rect().width() - 48.0).clamp(0.0, 320.0));
                    video_controls(ui, state, seek_position)
                })
                .inner
        })
        .inner
}

fn video_controls(
    ui: &mut egui::Ui,
    state: crate::media::PlaybackState,
    seek_position: &mut Option<f64>,
) -> Vec<crate::media::PlaybackCommand> {
    use crate::media::PlaybackCommand;
    let mut commands = Vec::new();
    ui.horizontal(|ui| {
        let side_size = egui::vec2(
            (ui.available_width() - 32.0 - 2.0 * ui.spacing().item_spacing.x).max(0.0) / 2.0,
            32.0,
        );
        ui.allocate_ui_with_layout(
            side_size,
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_size(side_size);
                ui.add(
                    egui::Label::new(format!(
                        "{} / {}",
                        playback_time(seek_position.or(state.position)),
                        playback_time(state.duration)
                    ))
                    .truncate(),
                );
            },
        );
        if play_pause_button(ui, state.paused).clicked() {
            commands.push(PlaybackCommand::Pause(!state.paused));
        }
        ui.allocate_ui_with_layout(
            side_size,
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.set_min_size(side_size);
                ui.spacing_mut().slider_width = 72.0;
                let mut volume = state.volume.clamp(0.0, 100.0);
                let response = ui
                    .add(
                        egui::Slider::new(&mut volume, 0.0..=100.0)
                            .show_value(false)
                            .fixed_decimals(0),
                    )
                    .on_hover_text(format!("Volume: {:.0}%", state.volume));
                if response.changed() {
                    commands.push(PlaybackCommand::Volume(volume));
                }
                ui.label("Volume");
            },
        );
    });
    let enabled = state.seekable && state.duration.is_some();
    let duration = state.duration.unwrap_or(1.0);
    let mut position = seek_position
        .or(state.position)
        .unwrap_or(0.0)
        .clamp(0.0, duration);
    let response = ui
        .scope(|ui| {
            ui.spacing_mut().slider_width = ui.available_width();
            ui.add_enabled(
                enabled,
                egui::Slider::new(&mut position, 0.0..=duration)
                    .show_value(false)
                    .trailing_fill(true),
            )
        })
        .inner;
    if response.dragged() {
        *seek_position = Some(position);
    }
    if enabled && (response.drag_stopped() || (response.changed() && !response.dragged())) {
        commands.push(PlaybackCommand::Seek(position));
        *seek_position = None;
    }
    if !enabled {
        *seek_position = None;
    }
    commands
}

fn play_pause_button(ui: &mut egui::Ui, paused: bool) -> egui::Response {
    let label = if paused { "Play" } else { "Pause" };
    let response = ui.add_sized(egui::vec2(32.0, 32.0), egui::Button::new(""));
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let center = response.rect.center();
    let color = ui.style().interact(&response).fg_stroke.color;
    if paused {
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                center + egui::vec2(-6.0, -7.0),
                center + egui::vec2(6.0, 0.0),
                center + egui::vec2(-6.0, 7.0),
            ],
            color,
            egui::Stroke::NONE,
        ));
    } else {
        for offset in [-4.0, 4.0] {
            ui.painter().rect_filled(
                egui::Rect::from_center_size(
                    center + egui::vec2(offset, 0.0),
                    egui::vec2(3.0, 14.0),
                ),
                0,
                color,
            );
        }
    }
    response.on_hover_text(label)
}

fn playback_time(seconds: Option<f64>) -> String {
    let Some(seconds) = seconds.filter(|value| value.is_finite() && *value >= 0.0) else {
        return "--:--".into();
    };
    let seconds = seconds.floor() as u64;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

fn update_texture(
    texture: &mut Option<egui::TextureHandle>,
    ctx: &egui::Context,
    image: egui::ColorImage,
) {
    if let Some(texture) = texture {
        texture.set(image, egui::TextureOptions::LINEAR);
    } else {
        *texture = Some(ctx.load_texture("media-preview", image, egui::TextureOptions::LINEAR));
    }
}

fn animation_at(frames: &[preview::AnimationFrame], elapsed: Duration) -> (usize, Duration) {
    let total: u128 = frames.iter().map(|frame| frame.delay.as_nanos()).sum();
    let mut position = elapsed.as_nanos() % total;
    for (index, frame) in frames.iter().enumerate() {
        let delay = frame.delay.as_nanos();
        if position < delay {
            return (index, Duration::from_nanos((delay - position) as u64));
        }
        position -= delay;
    }
    unreachable!("GIF decoding guarantees non-empty frames with nonzero delays")
}

impl eframe::App for PreviewApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(windows)]
        if let Some(monitor) = &self.outside_clicks {
            ctx.request_repaint_after(Duration::from_millis(16));
            if monitor.take_close_request() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
        }
        #[cfg(not(windows))]
        {
            let close = ctx.input(|input| {
                close_on_focus_loss(input.viewport().focused, &mut self.has_had_focus)
            });
            if close {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
        }
        #[cfg(windows)]
        self.handle_navigation(ctx);
        self.receive(ctx);
        self.update_media(ctx);
        // Do not close immediately from the Space press that opened the window.
        handle_close_keys(ctx, &mut self.close_keys_released);

        self.show_content(ctx);
        self.show_video_controls(ctx);
    }
}

impl PreviewApp {
    fn show_content(&self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::from_gray(240)))
            .show(ctx, |ui| {
                *ui.visuals_mut() = egui::Visuals::light();
                #[cfg(windows)]
                if let Some(error) = &self.navigation_error {
                    ui.colored_label(ui.visuals().warn_fg_color, error);
                }
                if let Some(error) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    return;
                }
                if let Some(error) = &self.control_error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                let Some(preview) = &self.preview else {
                    ui.spinner();
                    return;
                };
                match &preview.content {
                    Content::Image { .. } | Content::Animation(_) | Content::Video => {
                        if let Some(texture) = &self.texture {
                            let pane = ui.available_rect_before_wrap();
                            let size = texture.size_vec2();
                            let scale = (pane.width() / size.x).min(pane.height() / size.y);
                            let rect = egui::Rect::from_center_size(pane.center(), size * scale);
                            egui::Image::new(texture).paint_at(ui, rect);
                        }
                    }
                    Content::Text { text, .. } => {
                        egui::ScrollArea::both().id_salt(&self.path).show(ui, |ui| {
                            ui.add(
                                egui::Label::new(egui::RichText::new(text).monospace())
                                    .wrap_mode(egui::TextWrapMode::Extend),
                            );
                        });
                    }
                    Content::Directory { entries, .. } => {
                        if entries.is_empty() {
                            ui.label("Empty folder");
                        }
                        egui::ScrollArea::both().id_salt(&self.path).show(ui, |ui| {
                            for entry in entries {
                                ui.monospace(entry);
                            }
                        });
                    }
                    Content::Unsupported {
                        bytes, extension, ..
                    } => {
                        show_file_info(ui, *bytes, extension, self.texture.as_ref());
                    }
                }
            });
    }
}

fn show_file_info(
    ui: &mut egui::Ui,
    bytes: u64,
    extension: &str,
    icon: Option<&egui::TextureHandle>,
) {
    let center = ui.available_rect_before_wrap().center();
    let rect = egui::Rect::from_center_size(center - egui::vec2(0.0, 18.0), egui::vec2(96.0, 96.0));
    if let Some(icon) = icon {
        egui::Image::new(icon).paint_at(ui, rect);
    } else {
        let page = rect.shrink2(egui::vec2(14.0, 4.0));
        ui.painter().rect(
            page,
            6.0,
            ui.visuals().faint_bg_color,
            egui::Stroke::new(2.0, ui.visuals().text_color()),
            egui::StrokeKind::Inside,
        );
        let badge: String = extension.chars().take(8).collect();
        ui.painter().text(
            page.center(),
            egui::Align2::CENTER_CENTER,
            badge,
            egui::FontId::proportional(14.0),
            ui.visuals().text_color(),
        );
    }
    ui.painter().text(
        egui::pos2(center.x, rect.bottom() + 16.0),
        egui::Align2::CENTER_TOP,
        file_size(bytes),
        egui::FontId::proportional(16.0),
        ui.visuals().text_color(),
    );
}

fn file_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let units = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut size = bytes as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < units.len() {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", units[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_sizes_cover_bytes_and_binary_unit_boundaries() {
        assert_eq!(file_size(0), "0 B");
        assert_eq!(file_size(1), "1 B");
        assert_eq!(file_size(1023), "1023 B");
        assert_eq!(file_size(1024), "1.0 KiB");
        assert_eq!(file_size(1536), "1.5 KiB");
        assert_eq!(file_size(1024 * 1024), "1.0 MiB");
        assert_eq!(file_size(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(file_size(u64::MAX), "16.0 EiB");
    }

    #[test]
    fn unsupported_preview_only_displays_icon_and_size() {
        let ctx = egui::Context::default();
        let (mut app, _pending) = app_with_requests("hidden-path.pdf");
        app.accept_result(
            app.path.clone(),
            Ok(Preview {
                content: Content::Unsupported {
                    bytes: 1536,
                    extension: "PDF".into(),
                    icon: Some(crate::file_icon::FileIcon {
                        size: [1, 1],
                        rgba: vec![0, 120, 255, 255],
                    }),
                },
            }),
            &ctx,
        );
        let pane = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 640.0));
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(pane),
                ..Default::default()
            },
            |ctx| app.show_content(ctx),
        );
        let text: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect();
        assert_eq!(text, ["1.5 KiB"]);
        let texture_id = app.texture.as_ref().unwrap().id();
        let icon = output.shapes.iter().find(|shape| {
            matches!(&shape.shape, egui::Shape::Rect(rect) if rect.brush.as_ref().is_some_and(|brush| brush.fill_texture_id == texture_id))
        }).expect("Expected file-type icon");
        let bounds = icon.shape.visual_bounding_rect();
        assert_eq!(bounds.size(), egui::vec2(96.0, 96.0));
        assert_eq!(bounds.center().x, pane.center().x);
        app.request_path(PathBuf::from("next.txt"));
        assert!(app.texture.is_none());
    }

    #[test]
    fn generic_icon_has_extension_badge_and_size_without_error_text() {
        let ctx = egui::Context::default();
        let (mut app, _pending) = app_with_requests("unknown.archive");
        app.preview = Some(Preview {
            content: Content::Unsupported {
                bytes: 0,
                extension: "ARCHIVE".into(),
                icon: None,
            },
        });
        let output = ctx.run(Default::default(), |ctx| app.show_content(ctx));
        let text: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect();
        assert_eq!(text, ["ARCHIVE", "0 B"]);
    }

    #[test]
    fn focus_dismissal_does_not_close_before_initial_activation() {
        let mut has_had_focus = false;
        assert!(!close_on_focus_loss(None, &mut has_had_focus));
        assert!(!close_on_focus_loss(Some(false), &mut has_had_focus));
        assert!(!close_on_focus_loss(Some(true), &mut has_had_focus));
        assert!(!close_on_focus_loss(None, &mut has_had_focus));
        assert!(close_on_focus_loss(Some(false), &mut has_had_focus));
    }

    fn playing_state() -> crate::media::PlaybackState {
        crate::media::PlaybackState {
            paused: false,
            position: Some(20.0),
            duration: Some(100.0),
            seekable: true,
            volume: 70.0,
        }
    }

    fn controls_frame(
        ctx: &egui::Context,
        state: crate::media::PlaybackState,
        seek: &mut Option<f64>,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Vec<crate::media::PlaybackCommand>) {
        let mut commands = Vec::new();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 640.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                commands = video_controls_overlay(ctx, state, seek);
            },
        );
        (output, commands)
    }

    fn pointer_button(position: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn controls_visibility_frame(
        ctx: &egui::Context,
        last_activity: &mut Option<f64>,
        time: f64,
        events: Vec<egui::Event>,
    ) -> bool {
        let mut visible = false;
        let _ = ctx.run(
            egui::RawInput {
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| {
                visible = video_controls_visible(
                    ctx,
                    last_activity,
                    crate::settings::Settings::default().controls_hide_delay,
                )
            },
        );
        visible
    }

    #[test]
    fn video_controls_hide_after_two_seconds_and_reappear_on_input() {
        let ctx = egui::Context::default();
        let mut last_activity = None;
        assert!(controls_visibility_frame(
            &ctx,
            &mut last_activity,
            0.0,
            vec![]
        ));
        assert!(controls_visibility_frame(
            &ctx,
            &mut last_activity,
            1.99,
            vec![]
        ));
        assert!(!controls_visibility_frame(
            &ctx,
            &mut last_activity,
            2.0,
            vec![]
        ));
        assert!(controls_visibility_frame(
            &ctx,
            &mut last_activity,
            3.0,
            vec![egui::Event::PointerMoved(egui::pos2(40.0, 40.0))],
        ));
        assert!(!controls_visibility_frame(
            &ctx,
            &mut last_activity,
            5.0,
            vec![]
        ));
        assert!(controls_visibility_frame(
            &ctx,
            &mut last_activity,
            6.0,
            vec![key_event(egui::Key::Tab, true, false)],
        ));
        assert!(!controls_visibility_frame(
            &ctx,
            &mut last_activity,
            8.0,
            vec![]
        ));
    }

    #[test]
    fn video_controls_remain_visible_during_long_drags() {
        let ctx = egui::Context::default();
        let mut last_activity = None;
        let point = egui::pos2(40.0, 40.0);
        assert!(controls_visibility_frame(
            &ctx,
            &mut last_activity,
            0.0,
            pointer_button(point, true),
        ));
        assert!(controls_visibility_frame(
            &ctx,
            &mut last_activity,
            10.0,
            vec![]
        ));
        assert!(controls_visibility_frame(
            &ctx,
            &mut last_activity,
            11.0,
            pointer_button(point, false),
        ));
        assert!(!controls_visibility_frame(
            &ctx,
            &mut last_activity,
            13.0,
            vec![]
        ));
    }

    #[test]
    fn controls_use_the_configured_auto_hide_delay() {
        let ctx = egui::Context::default();
        let mut last_activity = None;
        for (time, expected) in [(0.0, true), (2.0, true), (4.99, true), (5.0, false)] {
            let _ = ctx.run(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ctx| {
                    assert_eq!(
                        video_controls_visible(ctx, &mut last_activity, 5.0),
                        expected
                    )
                },
            );
        }
    }

    #[test]
    fn bottom_centered_controls_are_compact_and_do_not_reserve_video_space() {
        for size in [egui::vec2(400.0, 280.0), egui::vec2(900.0, 640.0)] {
            let ctx = egui::Context::default();
            let pane = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            let mut seek = None;
            let mut output = egui::FullOutput::default();
            for _ in 0..2 {
                output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(pane),
                        ..Default::default()
                    },
                    |ctx| {
                        let available = ctx.available_rect();
                        video_controls_overlay(ctx, playing_state(), &mut seek);
                        assert_eq!(ctx.available_rect(), available);
                    },
                );
            }
            let controls = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect)
                        if rect.fill == egui::Color32::from_black_alpha(210) =>
                    {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .expect("Expected translucent controls background");
            assert!(controls.width() <= 360.0, "{controls:?}");
            assert!(controls.height() <= 100.0, "{controls:?}");
            assert!(pane.contains_rect(controls));
            assert!(
                (play_pause_icon_rect(&output, false).center().x - controls.center().x).abs() < 1.0,
                "Play/Pause must be centered in the controls"
            );
            assert!(
                (controls.center().x - pane.center().x).abs() < 1.0,
                "{controls:?}"
            );
            assert!(
                (controls.bottom() - (pane.bottom() - 12.0)).abs() < 1.0,
                "{controls:?}"
            );
        }
    }

    #[test]
    fn changing_files_resets_the_control_visibility_timer() {
        let (mut app, _pending) = app_with_requests("video.mp4");
        app.controls_last_activity = Some(1.0);
        app.request_path(PathBuf::from("next.mp4"));
        assert!(app.controls_last_activity.is_none());
    }

    #[test]
    fn playback_time_formats_loading_minutes_and_hours() {
        assert_eq!(playback_time(None), "--:--");
        assert_eq!(playback_time(Some(f64::NAN)), "--:--");
        assert_eq!(playback_time(Some(-1.0)), "--:--");
        assert_eq!(playback_time(Some(0.0)), "0:00");
        assert_eq!(playback_time(Some(125.9)), "2:05");
        assert_eq!(playback_time(Some(3661.0)), "1:01:01");
    }

    #[test]
    fn play_pause_button_issues_correct_command() {
        use crate::media::PlaybackCommand;
        for paused in [false, true] {
            let ctx = egui::Context::default();
            let state = crate::media::PlaybackState {
                paused,
                ..playing_state()
            };
            let mut seek = None;
            controls_frame(&ctx, state, &mut seek, vec![]);
            let (output, _) = controls_frame(&ctx, state, &mut seek, vec![]);
            let icon = play_pause_icon_rect(&output, paused);
            let position = icon.center();
            assert!((position.x - 450.0).abs() < 1.0, "{icon:?}");
            assert_eq!(icon.height(), 14.0);
            assert_eq!(icon.width(), if paused { 12.0 } else { 11.0 });
            assert!(!output.shapes.iter().any(|shape| matches!(
                &shape.shape,
                egui::Shape::Text(text) if matches!(text.galley.text(), "Play" | "Pause")
            )));
            controls_frame(&ctx, state, &mut seek, pointer_button(position, true));
            let (_, commands) =
                controls_frame(&ctx, state, &mut seek, pointer_button(position, false));
            assert!(
                matches!(commands.as_slice(), [PlaybackCommand::Pause(value)] if *value != paused),
                "{commands:?}"
            );
        }
    }

    fn play_pause_icon_rect(output: &egui::FullOutput, paused: bool) -> egui::Rect {
        if paused {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Path(path) if path.closed && path.points.len() == 3 => {
                        Some(shape.shape.visual_bounding_rect())
                    }
                    _ => None,
                })
                .expect("Expected play triangle")
        } else {
            let bars: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) if rect.rect.size() == egui::vec2(3.0, 14.0) => {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(bars.len(), 2, "Expected two pause bars");
            bars[0].union(bars[1])
        }
    }

    #[test]
    fn timeline_commits_seek_on_release_and_disables_unknown_duration() {
        use crate::media::PlaybackCommand;
        for available in [true, false] {
            let ctx = egui::Context::default();
            let state = crate::media::PlaybackState {
                duration: available.then_some(100.0),
                seekable: available,
                ..playing_state()
            };
            let mut seek = None;
            controls_frame(&ctx, state, &mut seek, vec![]);
            let (output, _) = controls_frame(&ctx, state, &mut seek, vec![]);
            let track = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect)
                        if rect.rect.width() > 250.0 && rect.rect.height() < 12.0 =>
                    {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .expect("Expected overlay-width timeline");
            let point = egui::pos2(track.left() + track.width() * 0.75, track.center().y);
            let (_, pressing) = controls_frame(&ctx, state, &mut seek, pointer_button(point, true));
            assert!(
                !pressing
                    .iter()
                    .any(|command| matches!(command, PlaybackCommand::Seek(_)))
            );
            let (_, releasing) =
                controls_frame(&ctx, state, &mut seek, pointer_button(point, false));
            if available {
                assert!(
                    matches!(releasing.as_slice(), [PlaybackCommand::Seek(value)] if (*value - 75.0).abs() < 2.0),
                    "{releasing:?}"
                );
            } else {
                assert!(releasing.is_empty());
            }
            assert!(seek.is_none());
        }
    }

    #[test]
    fn volume_slider_issues_volume_command() {
        use crate::media::PlaybackCommand;
        let ctx = egui::Context::default();
        let mut seek = None;
        let state = playing_state();
        controls_frame(&ctx, state, &mut seek, vec![]);
        let (output, _) = controls_frame(&ctx, state, &mut seek, vec![]);
        let track = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect)
                    if (50.0..100.0).contains(&rect.rect.width()) && rect.rect.height() < 12.0 =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .expect("Expected volume slider");
        let point = egui::pos2(track.left() + track.width() * 0.3, track.center().y);
        let (_, commands) = controls_frame(&ctx, state, &mut seek, pointer_button(point, true));
        assert!(commands.iter().any(|command| matches!(command, PlaybackCommand::Volume(value) if (0.0..=100.0).contains(value) && *value < 50.0)), "{commands:?}");
    }

    #[test]
    fn previews_stay_on_top_and_only_standalone_previews_activate() {
        for activate in [false, true] {
            let viewport = preview_viewport(activate);
            assert_eq!(viewport.active, Some(activate));
            assert_eq!(viewport.window_level, Some(egui::WindowLevel::AlwaysOnTop));
        }
    }

    fn key_event(key: egui::Key, pressed: bool, repeat: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn frame(ctx: &egui::Context, armed: &mut bool, events: Vec<egui::Event>) -> bool {
        let output = ctx.run(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ctx| handle_close_keys(ctx, armed),
        );
        output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|viewport| {
                viewport
                    .commands
                    .iter()
                    .any(|command| matches!(command, egui::ViewportCommand::Close))
            })
    }

    fn app_with_requests(path: &str) -> (PreviewApp, Receiver<PathBuf>) {
        let (requests, pending) = mpsc::channel();
        let (_sender, receiver) = mpsc::channel();
        (
            PreviewApp {
                settings: crate::settings::Settings::default(),
                path: PathBuf::from(path),
                requests,
                receiver,
                loading: false,
                preview: None,
                error: None,
                texture: None,
                close_keys_released: true,
                animation_started: None,
                animation_frame: None,
                video: None,
                seek_position: None,
                control_error: None,
                controls_last_activity: None,
                #[cfg(windows)]
                outside_clicks: None,
                #[cfg(not(windows))]
                has_had_focus: false,
                #[cfg(windows)]
                navigator: None,
                #[cfg(windows)]
                navigation_error: None,
            },
            pending,
        )
    }

    fn text_preview() -> Preview {
        Preview {
            content: Content::Text {
                text: "hello".into(),
                truncated: false,
            },
        }
    }

    fn animation_preview() -> Preview {
        Preview {
            content: Content::Animation(vec![
                preview::AnimationFrame {
                    rgba: vec![255, 0, 0, 255],
                    size: [1, 1],
                    delay: Duration::from_millis(40),
                },
                preview::AnimationFrame {
                    rgba: vec![0, 255, 0, 255],
                    size: [1, 1],
                    delay: Duration::from_millis(120),
                },
            ]),
        }
    }

    #[test]
    fn animation_timing_respects_each_frame_and_loops() {
        let Content::Animation(frames) = animation_preview().content else {
            unreachable!()
        };
        assert_eq!(
            animation_at(&frames, Duration::ZERO),
            (0, Duration::from_millis(40))
        );
        assert_eq!(
            animation_at(&frames, Duration::from_millis(40)),
            (1, Duration::from_millis(120))
        );
        assert_eq!(
            animation_at(&frames, Duration::from_millis(159)),
            (1, Duration::from_millis(1))
        );
        assert_eq!(
            animation_at(&frames, Duration::from_millis(160)),
            (0, Duration::from_millis(40))
        );
        assert_eq!(
            animation_at(&frames, Duration::from_millis(1000)),
            (1, Duration::from_millis(120))
        );
    }

    #[test]
    fn animation_uploads_new_frames_and_resets_on_navigation() {
        let ctx = egui::Context::default();
        let (mut app, _pending) = app_with_requests("animated.gif");
        app.accept_result(app.path.clone(), Ok(animation_preview()), &ctx);
        let mut frame = |elapsed| {
            app.animation_started = Some(Instant::now() - Duration::from_millis(elapsed));
            let output = ctx.run(Default::default(), |ctx| app.update_media(ctx));
            let delta = output
                .textures_delta
                .set
                .iter()
                .find(|(id, _)| *id == app.texture.as_ref().unwrap().id())
                .expect("Animation should update texture");
            let egui::ImageData::Color(image) = &delta.1.image;
            image.pixels[0]
        };
        assert_eq!(frame(0), egui::Color32::RED);
        assert_eq!(frame(50), egui::Color32::GREEN);
        app.load_path(PathBuf::from("next.txt"), &ctx);
        assert!(app.animation_started.is_none());
        assert!(app.animation_frame.is_none());
        assert!(app.texture.is_none());
        assert!(app.video.is_none());
    }

    #[test]
    #[ignore = "requires HOP_MPV_LIBRARY and HOP_TEST_VIDEO"]
    fn navigating_away_releases_active_video_and_audio() {
        fn frame(
            ctx: &egui::Context,
            app: &mut PreviewApp,
            events: Vec<egui::Event>,
        ) -> egui::FullOutput {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 640.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    app.update_media(ctx);
                    app.show_content(ctx);
                    app.show_video_controls(ctx);
                },
            )
        }
        let ctx = egui::Context::default();
        let path = PathBuf::from(std::env::var_os("HOP_TEST_VIDEO").expect("Set HOP_TEST_VIDEO"));
        let (mut app, _pending) = app_with_requests("video.mp4");
        app.settings.default_volume = 18.0;
        app.path = path;
        app.accept_result(
            app.path.clone(),
            Ok(Preview {
                content: Content::Video,
            }),
            &ctx,
        );
        assert!(app.error.is_none(), "{:?}", app.error);
        assert!(app.video.is_some());
        assert!((app.video.as_ref().unwrap().state().unwrap().volume - 18.0).abs() < 0.01);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            frame(&ctx, &mut app, vec![]);
            if app
                .video
                .as_ref()
                .unwrap()
                .state()
                .unwrap()
                .duration
                .is_some()
            {
                break;
            }
            assert!(Instant::now() < deadline, "Video metadata did not load");
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = frame(&ctx, &mut app, vec![]);
        let pause = play_pause_icon_rect(&output, false).center();
        frame(&ctx, &mut app, pointer_button(pause, true));
        frame(&ctx, &mut app, pointer_button(pause, false));
        assert!(app.video.as_ref().unwrap().state().unwrap().paused);
        let output = frame(&ctx, &mut app, vec![]);
        let timeline = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect)
                    if rect.rect.width() > 250.0 && rect.rect.height() < 12.0 =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .expect("Expected timeline");
        let point = egui::pos2(
            timeline.left() + timeline.width() * 0.6,
            timeline.center().y,
        );
        frame(&ctx, &mut app, pointer_button(point, true));
        frame(&ctx, &mut app, pointer_button(point, false));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            frame(&ctx, &mut app, vec![]);
            let state = app.video.as_ref().unwrap().state().unwrap();
            if (state.position.unwrap() - state.duration.unwrap() * 0.6).abs() < 0.2 {
                assert!(state.paused);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Timeline did not seek the actual player"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let volume = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect)
                    if (50.0..100.0).contains(&rect.rect.width()) && rect.rect.height() < 12.0 =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .expect("Expected volume slider");
        let point = egui::pos2(volume.left() + volume.width() * 0.3, volume.center().y);
        frame(&ctx, &mut app, pointer_button(point, true));
        frame(&ctx, &mut app, pointer_button(point, false));
        assert!(app.video.as_ref().unwrap().state().unwrap().volume < 50.0);
        assert!(app.error.is_none(), "{:?}", app.error);
        assert!(app.control_error.is_none(), "{:?}", app.control_error);
        app.load_path(PathBuf::from("next.txt"), &ctx);
        assert!(app.video.is_none());
        assert!(app.texture.is_none());
        assert!(app.loading);
        app.accept_result(app.path.clone(), Ok(text_preview()), &ctx);
        assert!(matches!(app.preview.unwrap().content, Content::Text { .. }));
    }

    #[test]
    fn preview_pane_is_light_grey_without_changing_the_global_theme() {
        for visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
            let ctx = egui::Context::default();
            ctx.set_visuals(visuals.clone());
            let (mut app, _pending) = app_with_requests("text.txt");
            app.preview = Some(text_preview());
            let pane = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 640.0));
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(pane),
                    ..Default::default()
                },
                |ctx| app.show_content(ctx),
            );
            assert!(output.shapes.iter().any(|shape| {
                matches!(
                    &shape.shape,
                    egui::Shape::Rect(rect)
                        if rect.rect == pane && rect.fill == egui::Color32::from_gray(240)
                )
            }));
            let text = output
                .shapes
                .iter()
                .find_map(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape {
                        Some(text)
                    } else {
                        None
                    }
                })
                .unwrap();
            assert_eq!(text.fallback_color, egui::Visuals::light().text_color());
            assert_eq!(ctx.style().visuals.dark_mode, visuals.dark_mode);
        }
    }

    #[test]
    fn text_preview_has_no_header_details_or_hints() {
        let ctx = egui::Context::default();
        let (mut app, _pending) = app_with_requests("secret-file-name.txt");
        app.preview = Some(text_preview());
        let pane = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 640.0));
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(pane),
                ..Default::default()
            },
            |ctx| {
                app.show_content(ctx);
                assert_eq!(ctx.used_rect(), pane);
            },
        );
        let text: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    Some(text.galley.text())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(text, ["hello"]);
    }

    #[test]
    fn image_uses_full_pane_and_preserves_aspect_ratio() {
        let ctx = egui::Context::default();
        let (mut app, _pending) = app_with_requests("image.png");
        app.accept_result(
            app.path.clone(),
            Ok(Preview {
                content: Content::Image {
                    pixels: vec![egui::Color32::WHITE; 2],
                    size: [2, 1],
                },
            }),
            &ctx,
        );
        let texture_id = app.texture.as_ref().unwrap().id();
        let pane = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 640.0));
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(pane),
                ..Default::default()
            },
            |ctx| app.show_content(ctx),
        );
        let image = output.shapes.iter().find(|shape| {
            matches!(&shape.shape, egui::Shape::Rect(rect) if rect.brush.as_ref().is_some_and(|brush| brush.fill_texture_id == texture_id))
        })        .expect("Expected textured image rectangle");
        let bounds = image.shape.visual_bounding_rect();
        assert_eq!(bounds.width(), 900.0);
        assert_eq!(bounds.height(), 450.0);
        assert_eq!(bounds.center(), pane.center());
        assert!(
            !output
                .shapes
                .iter()
                .any(|shape| matches!(shape.shape, egui::Shape::Text(_)))
        );
    }

    #[test]
    fn file_navigation_takes_priority_over_a_focused_playback_slider() {
        let ctx = egui::Context::default();
        let mut volume = 70.0;
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add(egui::Slider::new(&mut volume, 0.0..=100.0))
                    .request_focus();
            });
        });
        let mut keys = Vec::new();
        let _ = ctx.run(
            egui::RawInput {
                events: vec![
                    key_event(egui::Key::ArrowRight, true, false),
                    key_event(egui::Key::ArrowRight, true, true),
                    key_event(egui::Key::ArrowDown, true, false),
                ],
                ..Default::default()
            },
            |ctx| {
                assert!(
                    ctx.wants_keyboard_input(),
                    "The playback slider must have focus"
                );
                keys = take_navigation_keys(ctx);
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add(egui::Slider::new(&mut volume, 0.0..=100.0));
                });
            },
        );
        assert_eq!(
            keys,
            [
                egui::Key::ArrowRight,
                egui::Key::ArrowRight,
                egui::Key::ArrowDown
            ]
        );
        assert_eq!(
            volume, 70.0,
            "Navigation keys must not also adjust the slider"
        );
    }

    #[test]
    fn arrows_are_consumed_in_order_including_repeats_but_not_modifiers() {
        let ctx = egui::Context::default();
        let mut modified = key_event(egui::Key::ArrowLeft, true, false);
        if let egui::Event::Key { modifiers, .. } = &mut modified {
            *modifiers = egui::Modifiers::CTRL;
        }
        let mut keys = Vec::new();
        let _ = ctx.run(
            egui::RawInput {
                events: vec![
                    key_event(egui::Key::ArrowRight, true, false),
                    key_event(egui::Key::ArrowRight, true, true),
                    key_event(egui::Key::ArrowDown, true, false),
                    key_event(egui::Key::ArrowUp, true, false),
                    key_event(egui::Key::ArrowLeft, true, false),
                    key_event(egui::Key::ArrowLeft, false, false),
                    modified,
                    key_event(egui::Key::Escape, true, false),
                ],
                ..Default::default()
            },
            |ctx| {
                keys = take_navigation_keys(ctx);
                ctx.input(|input| {
                    assert!(input.events.iter().any(|event| matches!(
                        event,
                        egui::Event::Key {
                            key: egui::Key::Escape,
                            ..
                        }
                    )));
                    assert!(input.events.iter().any(|event| matches!(
                        event,
                        egui::Event::Key { modifiers, .. } if modifiers.ctrl
                    )));
                });
            },
        );
        assert_eq!(
            keys,
            [
                egui::Key::ArrowRight,
                egui::Key::ArrowRight,
                egui::Key::ArrowDown,
                egui::Key::ArrowUp,
                egui::Key::ArrowLeft
            ]
        );
    }

    #[test]
    fn reloading_resets_old_state_and_updates_window_title() {
        let ctx = egui::Context::default();
        let (mut app, pending) = app_with_requests("old.txt");
        app.preview = Some(text_preview());
        app.error = Some("old error".into());
        app.texture = Some(ctx.load_texture(
            "old-image",
            egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 255]),
            Default::default(),
        ));
        let output = ctx.run(Default::default(), |ctx| {
            app.load_path(PathBuf::from("new.txt"), ctx);
        });
        assert_eq!(pending.try_recv().unwrap(), PathBuf::from("new.txt"));
        assert!(app.loading);
        assert!(app.preview.is_none());
        assert!(app.error.is_none());
        assert!(app.texture.is_none());
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .any(|command| matches!(
                    command,
                    egui::ViewportCommand::Title(title) if title == "Hop - new.txt"
                ))
        );
    }

    #[test]
    fn stale_load_results_cannot_replace_new_selection() {
        let ctx = egui::Context::default();
        let (mut app, _pending) = app_with_requests("old.txt");
        app.load_path(PathBuf::from("new.txt"), &ctx);
        app.accept_result(PathBuf::from("old.txt"), Ok(text_preview()), &ctx);
        app.accept_result(PathBuf::from("old.txt"), Err("stale error".into()), &ctx);
        assert!(app.loading);
        assert!(app.preview.is_none());
        assert!(app.error.is_none());
        app.accept_result(app.path.clone(), Ok(text_preview()), &ctx);
        assert!(!app.loading);
        assert_eq!(app.path, PathBuf::from("new.txt"));
        assert!(
            matches!(app.preview.unwrap().content, Content::Text { text, .. } if text == "hello")
        );
    }

    #[test]
    fn can_navigate_after_a_preview_error_and_reports_worker_failure() {
        let ctx = egui::Context::default();
        let (mut app, pending) = app_with_requests("bad.png");
        app.accept_result(app.path.clone(), Err("Cannot decode image".into()), &ctx);
        assert!(app.error.is_some());
        app.load_path(PathBuf::from("next.txt"), &ctx);
        assert!(app.error.is_none());
        assert!(app.loading);
        drop(pending);
        app.load_path(PathBuf::from("another.txt"), &ctx);
        assert!(!app.loading);
        assert!(app.error.unwrap().contains("Cannot request file preview"));
    }

    #[test]
    fn worker_wakes_ui_after_publishing_ready_images_and_errors() {
        let directory = tempfile::tempdir().unwrap();
        let image_path = directory.path().join("ready.png");
        let rgba = [10, 20, 30, 255, 100, 150, 200, 128, 255, 120, 60, 0];
        image::RgbaImage::from_raw(3, 1, rgba.to_vec())
            .unwrap()
            .save(&image_path)
            .unwrap();
        let corrupt_path = directory.path().join("corrupt.png");
        std::fs::write(&corrupt_path, "broken").unwrap();

        for path in [
            image_path.clone(),
            corrupt_path,
            directory.path().join("missing.png"),
        ] {
            let ctx = egui::Context::default();
            for _ in 0..5 {
                let _ = ctx.run(Default::default(), |_| {});
            }
            let (wake, repaint) = mpsc::channel();
            ctx.set_request_repaint_callback(move |info| {
                wake.send((std::thread::current().id(), info)).unwrap();
            });
            let mut app = PreviewApp::new(path.clone(), crate::settings::Settings::default(), &ctx);
            let (worker, info) = repaint.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_ne!(worker, std::thread::current().id());
            assert_eq!(info.delay, Duration::ZERO);
            assert_eq!(info.viewport_id, egui::ViewportId::ROOT);
            let (loaded_path, result) = app.receiver.try_recv().unwrap();
            assert_eq!(loaded_path, path);
            app.accept_result(loaded_path, result, &ctx);
            let output = ctx.run(Default::default(), |_| {});
            assert!(!app.loading);
            if path == image_path {
                assert!(app.error.is_none());
                let texture = app.texture.as_ref().unwrap();
                let delta = output
                    .textures_delta
                    .set
                    .iter()
                    .find(|(id, _)| *id == texture.id())
                    .unwrap();
                let egui::ImageData::Color(image) = &delta.1.image;
                assert_eq!(image.size, [3, 1]);
                assert_eq!(
                    image.pixels,
                    egui::ColorImage::from_rgba_unmultiplied([3, 1], &rgba).pixels
                );
                assert!(matches!(
                    &app.preview.as_ref().unwrap().content,
                    Content::Image { pixels, .. } if pixels.is_empty()
                ));
            } else {
                assert!(app.error.is_some());
                assert!(app.texture.is_none());
            }
        }
    }

    #[test]
    fn worker_loads_multiple_files_without_reopening_the_app() {
        let ctx = egui::Context::default();
        let executable = std::env::current_exe().unwrap();
        let folder = executable.parent().unwrap().to_owned();
        let mut app = PreviewApp::new(
            executable.clone(),
            crate::settings::Settings::default(),
            &ctx,
        );
        let (path, result) = app.receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(path, executable);
        assert!(result.is_ok());
        app.load_path(folder.clone(), &ctx);
        let (path, result) = app.receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(path, folder);
        app.accept_result(path, result, &ctx);
        assert!(matches!(
            app.preview.unwrap().content,
            Content::Directory { .. }
        ));
    }

    #[test]
    fn ignores_opening_press_and_repeat_until_release() {
        let ctx = egui::Context::default();
        let mut armed = false;
        assert!(!frame(
            &ctx,
            &mut armed,
            vec![key_event(egui::Key::Space, true, false)]
        ));
        assert!(!frame(
            &ctx,
            &mut armed,
            vec![key_event(egui::Key::Space, true, true)]
        ));
        assert!(!frame(
            &ctx,
            &mut armed,
            vec![key_event(egui::Key::Space, false, false)]
        ));
        assert!(frame(
            &ctx,
            &mut armed,
            vec![key_event(egui::Key::Space, true, false)]
        ));
    }

    #[test]
    fn escape_closes_after_initial_idle_frame() {
        let ctx = egui::Context::default();
        let mut armed = false;
        assert!(!frame(&ctx, &mut armed, vec![]));
        assert!(frame(
            &ctx,
            &mut armed,
            vec![key_event(egui::Key::Escape, true, false)]
        ));
    }
}
