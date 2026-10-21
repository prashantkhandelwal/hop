use libloading::Library;
use std::ffi::{CStr, CString, c_char, c_int, c_ulong, c_void};
use std::path::Path;
use std::ptr;

#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}

impl RenderParam {
    fn new(kind: c_int, data: *mut c_void) -> Self {
        Self { kind, data }
    }

    fn end() -> Self {
        Self::new(0, ptr::null_mut())
    }
}

#[repr(C)]
struct Event {
    id: c_int,
    error: c_int,
    userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct EndFile {
    reason: c_int,
    error: c_int,
}

#[repr(C)]
struct LogMessage {
    prefix: *const c_char,
    level: *const c_char,
    text: *const c_char,
    log_level: c_int,
}

struct Api {
    create: unsafe extern "C" fn() -> *mut c_void,
    initialize: unsafe extern "C" fn(*mut c_void) -> c_int,
    option: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    command: unsafe extern "C" fn(*mut c_void, *mut *const c_char) -> c_int,
    get_property: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int,
    set_property: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int,
    wait_event: unsafe extern "C" fn(*mut c_void, f64) -> *mut Event,
    request_logs: unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int,
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    destroy: unsafe extern "C" fn(*mut c_void),
    render_create: unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *mut RenderParam) -> c_int,
    render_update: unsafe extern "C" fn(*mut c_void) -> u64,
    render: unsafe extern "C" fn(*mut c_void, *mut RenderParam) -> c_int,
    render_free: unsafe extern "C" fn(*mut c_void),
    _library: Library,
}

impl Api {
    fn load() -> Result<Self, String> {
        let library =
            if let Some(path) = std::env::var_os("HOP_MPV_LIBRARY") {
                // An explicit override must fail visibly rather than load another version.
                unsafe { Library::new(&path) }
                    .map_err(|error| format!("Cannot load HOP_MPV_LIBRARY: {error}"))?
            } else {
                #[cfg(windows)]
                let names = ["libmpv-2.dll", "mpv-2.dll"];
                #[cfg(not(windows))]
                let names = ["libmpv.so.2", "libmpv.so"];
                let executable = std::env::current_exe().map_err(|error| error.to_string())?;
                let directory = executable
                    .parent()
                    .ok_or("Cannot find executable directory.")?;
                let mut library = None;
                let mut errors = Vec::new();
                for name in names {
                    let paths = [directory.join(name), name.into()];
                    for path in paths {
                        match unsafe { Library::new(&path) } {
                            Ok(loaded) => {
                                library = Some(loaded);
                                break;
                            }
                            Err(error) => errors.push(format!("{}: {error}", path.display())),
                        }
                    }
                    if library.is_some() {
                        break;
                    }
                }
                library.ok_or_else(|| format!(
                "Video playback requires libmpv (API 2). Install libmpv.so.2 on Linux or put \
                 libmpv-2.dll beside Hop on Windows. You can also set HOP_MPV_LIBRARY \
                 to its full path.\n{}", errors.join("\n")
            ))?
            };
        // Signatures and struct layouts follow libmpv's stable client/render C API.
        unsafe {
            macro_rules! symbol {
                ($name:literal) => {
                    *library
                        .get(concat!($name, "\0").as_bytes())
                        .map_err(|error| format!("Incompatible libmpv ({}): {error}", $name))?
                };
            }
            let version: unsafe extern "C" fn() -> c_ulong = symbol!("mpv_client_api_version");
            if version() >> 16 != 2 {
                return Err("Video playback requires libmpv client API major version 2.".into());
            }
            Ok(Self {
                create: symbol!("mpv_create"),
                initialize: symbol!("mpv_initialize"),
                option: symbol!("mpv_set_option_string"),
                command: symbol!("mpv_command"),
                get_property: symbol!("mpv_get_property"),
                set_property: symbol!("mpv_set_property"),
                wait_event: symbol!("mpv_wait_event"),
                request_logs: symbol!("mpv_request_log_messages"),
                error_string: symbol!("mpv_error_string"),
                destroy: symbol!("mpv_terminate_destroy"),
                render_create: symbol!("mpv_render_context_create"),
                render_update: symbol!("mpv_render_context_update"),
                render: symbol!("mpv_render_context_render"),
                render_free: symbol!("mpv_render_context_free"),
                _library: library,
            })
        }
    }

    fn check(&self, result: c_int, action: &str) -> Result<(), String> {
        if result >= 0 {
            return Ok(());
        }
        let message = unsafe { CStr::from_ptr((self.error_string)(result)) }.to_string_lossy();
        Err(format!("{action}: {message}"))
    }
}

pub struct VideoPlayer {
    api: Api,
    handle: *mut c_void,
    renderer: *mut c_void,
    // u32 storage guarantees the pixel alignment required by libmpv's rgb0 output.
    pixels: Vec<u32>,
    size: [usize; 2],
}

#[derive(Clone, Copy)]
pub struct PlaybackState {
    pub paused: bool,
    pub position: Option<f64>,
    pub duration: Option<f64>,
    pub seekable: bool,
    pub volume: f64,
}

#[derive(Debug)]
pub enum PlaybackCommand {
    Pause(bool),
    Seek(f64),
    Volume(f64),
}

impl VideoPlayer {
    fn number(&self, name: &CStr) -> Result<Option<f64>, String> {
        let mut value = 0.0f64;
        let result = unsafe {
            (self.api.get_property)(
                self.handle,
                name.as_ptr(),
                5,
                (&mut value as *mut f64).cast(),
            )
        };
        // Metadata may be unavailable while libmpv is loading or for unindexed streams.
        if result == -10 {
            return Ok(None);
        }
        self.api
            .check(result, &format!("Cannot read {}", name.to_string_lossy()))?;
        if !value.is_finite() {
            return Err(format!(
                "Invalid video property: {}",
                name.to_string_lossy()
            ));
        }
        Ok(Some(value))
    }

    fn flag(&self, name: &CStr) -> Result<Option<bool>, String> {
        let mut value: c_int = 0;
        let result = unsafe {
            (self.api.get_property)(
                self.handle,
                name.as_ptr(),
                3,
                (&mut value as *mut c_int).cast(),
            )
        };
        if result == -10 {
            return Ok(None);
        }
        self.api
            .check(result, &format!("Cannot read {}", name.to_string_lossy()))?;
        Ok(Some(value != 0))
    }

    pub fn state(&self) -> Result<PlaybackState, String> {
        Ok(PlaybackState {
            paused: self
                .flag(c"pause")?
                .ok_or("Video pause state is unavailable.")?,
            position: self.number(c"time-pos")?,
            duration: self.number(c"duration")?.filter(|duration| *duration > 0.0),
            seekable: self.flag(c"seekable")?.unwrap_or(false),
            volume: self
                .number(c"volume")?
                .ok_or("Video volume is unavailable.")?,
        })
    }

    pub fn control(&mut self, command: PlaybackCommand) -> Result<(), String> {
        match command {
            PlaybackCommand::Pause(paused) => {
                let mut value = c_int::from(paused);
                self.api.check(
                    unsafe {
                        (self.api.set_property)(
                            self.handle,
                            c"pause".as_ptr(),
                            3,
                            (&mut value as *mut c_int).cast(),
                        )
                    },
                    "Cannot change playback state",
                )
            }
            PlaybackCommand::Volume(mut volume) => {
                Self::validate_range(volume, 100.0, "volume")?;
                self.api.check(
                    unsafe {
                        (self.api.set_property)(
                            self.handle,
                            c"volume".as_ptr(),
                            5,
                            (&mut volume as *mut f64).cast(),
                        )
                    },
                    "Cannot change volume",
                )
            }
            PlaybackCommand::Seek(position) => {
                let state = self.state()?;
                if !state.seekable {
                    return Err("This video is not seekable.".into());
                }
                let duration = state
                    .duration
                    .ok_or("Video duration is not available yet.")?;
                Self::validate_range(position, duration, "seek position")?;
                let value =
                    CString::new(position.to_string()).map_err(|error| error.to_string())?;
                let mut args = [
                    c"seek".as_ptr(),
                    value.as_ptr(),
                    c"absolute+exact".as_ptr(),
                    ptr::null(),
                ];
                self.api.check(
                    unsafe { (self.api.command)(self.handle, args.as_mut_ptr()) },
                    "Cannot seek video",
                )
            }
        }
    }

    pub fn open(path: &Path, initial_volume: f64) -> Result<Self, String> {
        Self::validate_range(initial_volume, 100.0, "initial volume")?;
        let api = Api::load()?;
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            return Err("Cannot create video player.".into());
        }
        let mut player = Self {
            api,
            handle,
            renderer: ptr::null_mut(),
            pixels: Vec::new(),
            size: [0, 0],
        };
        for (name, value) in [
            (c"config", c"no"),
            (c"load-scripts", c"no"),
            (c"ytdl", c"no"),
            (c"terminal", c"no"),
            (c"vo", c"libmpv"),
            (c"hwdec", c"no"),
            (c"loop-file", c"inf"),
            (c"keep-open", c"yes"),
            (c"audio-display", c"no"),
        ] {
            let status = unsafe { (player.api.option)(handle, name.as_ptr(), value.as_ptr()) };
            player.api.check(
                status,
                &format!("Cannot set video option {}", name.to_string_lossy()),
            )?;
        }
        let volume = CString::new(initial_volume.to_string())
            .map_err(|error| format!("Invalid initial volume: {error}"))?;
        player.api.check(
            unsafe { (player.api.option)(handle, c"volume".as_ptr(), volume.as_ptr()) },
            "Cannot set initial video volume",
        )?;
        player.api.check(
            unsafe { (player.api.initialize)(handle) },
            "Cannot initialize video/audio playback",
        )?;
        player.api.check(
            unsafe { (player.api.request_logs)(handle, c"error".as_ptr()) },
            "Cannot enable video/audio error reporting",
        )?;
        let mut params = [
            RenderParam::new(1, c"sw".as_ptr().cast_mut().cast()),
            RenderParam::end(),
        ];
        let status = unsafe {
            (player.api.render_create)(&mut player.renderer, handle, params.as_mut_ptr())
        };
        player
            .api
            .check(status, "Cannot initialize in-pane video rendering")?;
        let path =
            std::fs::canonicalize(path).map_err(|error| format!("Cannot open video: {error}"))?;
        let path = path.to_str().ok_or("libmpv requires a UTF-8 video path.")?;
        let filename =
            CString::new(path).map_err(|error| format!("Invalid video path: {error}"))?;
        let mut args = [c"loadfile".as_ptr(), filename.as_ptr(), ptr::null()];
        player.api.check(
            unsafe { (player.api.command)(handle, args.as_mut_ptr()) },
            "Cannot load video",
        )?;
        Ok(player)
    }

    fn validate_range(value: f64, maximum: f64, name: &str) -> Result<(), String> {
        if !value.is_finite() || !(0.0..=maximum).contains(&value) {
            return Err(format!(
                "Invalid {name}: expected a finite value between 0 and {maximum}."
            ));
        }
        Ok(())
    }

    pub fn render(&mut self, size: [usize; 2]) -> Result<Option<eframe::egui::ColorImage>, String> {
        // Drain events before rendering so corrupt/unsupported files fail explicitly.
        for _ in 0..100 {
            let event = unsafe { (self.api.wait_event)(self.handle, 0.0) };
            if event.is_null() {
                return Err("Video player returned an invalid event.".into());
            }
            let event = unsafe { &*event };
            if event.id == 0 {
                break;
            }
            self.api.check(event.error, "Video playback failed")?;
            if event.id == 2 && !event.data.is_null() {
                let log = unsafe { &*event.data.cast::<LogMessage>() };
                let prefix = unsafe { CStr::from_ptr(log.prefix) }.to_string_lossy();
                let text = unsafe { CStr::from_ptr(log.text) }.to_string_lossy();
                eprintln!("Hop video ({prefix}): {}", text.trim());
                if prefix == "ao" || prefix.starts_with("ao/") {
                    return Err(format!("Audio output failed: {}", text.trim()));
                }
            }
            if event.id == 7 && !event.data.is_null() {
                // Only the stable prefix is needed; later libmpv versions append fields.
                let end = unsafe { &*event.data.cast::<EndFile>() };
                self.api.check(end.error, "Cannot play this video")?;
            }
        }
        let size = render_size(size);
        let resized = self.size != size;
        let flags = unsafe { (self.api.render_update)(self.renderer) };
        if !resized && flags & 1 == 0 {
            return Ok(None);
        }
        self.size = size;
        self.pixels.resize(size[0] * size[1], 0);
        let mut dimensions = [size[0] as c_int, size[1] as c_int];
        let mut stride = size[0] * 4;
        let mut params = [
            RenderParam::new(17, dimensions.as_mut_ptr().cast()),
            RenderParam::new(18, c"rgb0".as_ptr().cast_mut().cast()),
            RenderParam::new(19, (&mut stride as *mut usize).cast()),
            RenderParam::new(20, self.pixels.as_mut_ptr().cast()),
            RenderParam::end(),
        ];
        let result = unsafe { (self.api.render)(self.renderer, params.as_mut_ptr()) };
        self.api.check(result, "Cannot render video frame")?;
        let pixels = self
            .pixels
            .iter()
            .map(|pixel| {
                let bytes = pixel.to_ne_bytes();
                eframe::egui::Color32::from_rgb(bytes[0], bytes[1], bytes[2])
            })
            .collect();
        Ok(Some(eframe::egui::ColorImage::new(size, pixels)))
    }
}

fn render_size(size: [usize; 2]) -> [usize; 2] {
    let width = size[0].max(1) as f64;
    let height = size[1].max(1) as f64;
    let scale = (1920.0 / width).min(1080.0 / height).min(1.0);
    [
        (width * scale).round().max(1.0) as usize,
        (height * scale).round().max(1.0) as usize,
    ]
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        unsafe {
            if !self.renderer.is_null() {
                (self.api.render_free)(self.renderer);
            }
            (self.api.destroy)(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_control_values() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 101.0] {
            assert!(VideoPlayer::validate_range(value, 100.0, "volume").is_err());
        }
        for value in [0.0, 70.0, 100.0] {
            assert!(VideoPlayer::validate_range(value, 100.0, "volume").is_ok());
        }
    }

    #[test]
    fn render_buffers_are_bounded_and_keep_aspect_ratio() {
        assert_eq!(render_size([0, 0]), [1, 1]);
        assert_eq!(render_size([900, 640]), [900, 640]);
        assert_eq!(render_size([3840, 2160]), [1920, 1080]);
        assert_eq!(render_size([1000, 4000]), [270, 1080]);
    }

    #[test]
    #[ignore = "requires HOP_MPV_LIBRARY and HOP_TEST_VIDEO with a video/audio fixture"]
    fn decodes_real_video_and_releases_player() {
        let path = std::env::var_os("HOP_TEST_VIDEO").expect("Set HOP_TEST_VIDEO");
        let mut player = VideoPlayer::open(Path::new(&path), 35.0).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut distinct = Vec::new();
        while std::time::Instant::now() < deadline {
            if let Some(image) = player.render([320, 180]).unwrap()
                && image.pixels.iter().any(|pixel| pixel.r() > 50)
            {
                distinct.push(image);
                if distinct.len() >= 3 {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            distinct.len() >= 3,
            "Expected multiple decoded video frames"
        );
        assert!(
            distinct
                .windows(2)
                .any(|pair| pair[0].pixels != pair[1].pixels)
        );
        // A decoded audio stream alone is not enough: confirm an active audio output.
        unsafe {
            let mut sample_rate: i64 = 0;
            let status = (player.api.get_property)(
                player.handle,
                c"audio-out-params/samplerate".as_ptr(),
                4,
                (&mut sample_rate as *mut i64).cast(),
            );
            player
                .api
                .check(status, "Audio output did not initialize")
                .unwrap();
            assert!(sample_rate > 0);
        }
        let state = player.state().unwrap();
        assert!((state.volume - 35.0).abs() < 0.01);
        let duration = state.duration.expect("Expected a known duration");
        assert!(state.seekable);
        assert!(!state.paused);
        player.control(PlaybackCommand::Pause(true)).unwrap();
        assert!(player.state().unwrap().paused);
        let paused_at = player.state().unwrap().position.unwrap();
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert!((player.state().unwrap().position.unwrap() - paused_at).abs() < 0.1);
        let target = duration * 0.6;
        player.control(PlaybackCommand::Seek(target)).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while (player.state().unwrap().position.unwrap() - target).abs() > 0.1 {
            player.render([320, 180]).unwrap();
            assert!(
                std::time::Instant::now() < deadline,
                "Seek did not reach target"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            player.state().unwrap().paused,
            "Seeking must not resume playback"
        );
        for volume in [0.0, 35.0, 100.0] {
            player.control(PlaybackCommand::Volume(volume)).unwrap();
            assert!((player.state().unwrap().volume - volume).abs() < 0.01);
        }
        assert!(player.control(PlaybackCommand::Volume(f64::NAN)).is_err());
        assert!(player.control(PlaybackCommand::Seek(-1.0)).is_err());
        assert!(
            player
                .control(PlaybackCommand::Seek(duration + 1.0))
                .is_err()
        );
        player.control(PlaybackCommand::Volume(0.0)).unwrap();
        player.control(PlaybackCommand::Pause(false)).unwrap();
        assert!(!player.state().unwrap().paused);
        let resumed_at = player.state().unwrap().position.unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while player.state().unwrap().position.unwrap() <= resumed_at + 0.04 {
            player.render([320, 180]).unwrap();
            assert!(
                std::time::Instant::now() < deadline,
                "Play did not resume playback"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        drop(player);
        let _reopened = VideoPlayer::open(Path::new(&path), 70.0).unwrap();
    }
}
