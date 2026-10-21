use image::{AnimationDecoder, ImageDecoder};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::Duration;

pub const TEXT_LIMIT: usize = 256 * 1024;
pub const DIRECTORY_LIMIT: usize = 200;
const IMAGE_FILE_LIMIT: u64 = 64 * 1024 * 1024;
const IMAGE_MEMORY_LIMIT: u64 = 256 * 1024 * 1024;
const ANIMATION_MEMORY_LIMIT: usize = 128 * 1024 * 1024;
const ANIMATION_FRAME_LIMIT: usize = 500;

pub struct AnimationFrame {
    pub rgba: Vec<u8>,
    pub size: [usize; 2],
    pub delay: Duration,
}

pub struct Preview {
    pub content: Content,
}

pub enum Content {
    Animation(Vec<AnimationFrame>),
    Video,
    Image {
        pixels: Vec<eframe::egui::Color32>,
        size: [usize; 2],
    },
    Text {
        text: String,
        truncated: bool,
    },
    Directory {
        entries: Vec<String>,
        truncated: bool,
    },
    Unsupported {
        bytes: u64,
        extension: String,
        icon: Option<crate::file_icon::FileIcon>,
    },
}

pub fn load(path: &Path) -> Result<Preview, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("Cannot inspect file: {error}"))?;
    let content = if metadata.is_dir() {
        load_directory(path)?
    } else if !metadata.is_file() {
        unsupported(path, metadata.len())
    } else if is_video(path) {
        Content::Video
    } else if is_image(path) {
        if metadata.len() > IMAGE_FILE_LIMIT {
            eprintln!(
                "Hop: {}: image exceeds the 64 MiB preview limit.",
                path.display()
            );
            unsupported(path, metadata.len())
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"))
        {
            load_gif(path)?
        } else {
            load_image(path)?
        }
    } else if is_document_or_binary(path) {
        unsupported(path, metadata.len())
    } else {
        load_text(path)?.unwrap_or_else(|| unsupported(path, metadata.len()))
    };
    Ok(Preview { content })
}

fn unsupported(path: &Path, bytes: u64) -> Content {
    Content::Unsupported {
        bytes,
        extension: path
            .extension()
            .map(|extension| extension.to_string_lossy().to_uppercase())
            .unwrap_or_else(|| "FILE".into()),
        icon: crate::file_icon::load(path),
    }
}

fn is_document_or_binary(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "pdf"
                    | "doc"
                    | "docx"
                    | "xls"
                    | "xlsx"
                    | "ppt"
                    | "pptx"
                    | "odt"
                    | "ods"
                    | "odp"
                    | "rtf"
                    | "epub"
                    | "zip"
                    | "7z"
                    | "rar"
                    | "gz"
                    | "bz2"
                    | "xz"
                    | "tar"
                    | "iso"
                    | "exe"
                    | "dll"
                    | "msi"
                    | "so"
                    | "bin"
                    | "dat"
                    | "mp3"
                    | "wav"
                    | "flac"
                    | "ogg"
                    | "m4a"
                    | "aac"
                    | "opus"
                    | "ttf"
                    | "otf"
                    | "woff"
                    | "woff2"
                    | "psd"
                    | "ai"
            )
        })
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "mp4"
                    | "m4v"
                    | "mkv"
                    | "webm"
                    | "mov"
                    | "avi"
                    | "wmv"
                    | "mpg"
                    | "mpeg"
                    | "mts"
                    | "m2ts"
                    | "ts"
                    | "ogv"
                    | "flv"
                    | "3gp"
            )
        })
}

fn image_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(IMAGE_MEMORY_LIMIT);
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits
}

fn load_gif(path: &Path) -> Result<Content, String> {
    let file = File::open(path).map_err(|error| format!("Cannot open GIF: {error}"))?;
    let mut decoder = image::codecs::gif::GifDecoder::new(BufReader::new(file))
        .map_err(|error| format!("Cannot decode GIF: {error}"))?;
    decoder
        .set_limits(image_limits())
        .map_err(|error| format!("GIF exceeds decoding limits: {error}"))?;
    collect_gif_frames(
        decoder.into_frames(),
        ANIMATION_FRAME_LIMIT,
        ANIMATION_MEMORY_LIMIT,
    )
}

fn collect_gif_frames(
    decoded: impl IntoIterator<Item = image::ImageResult<image::Frame>>,
    frame_limit: usize,
    memory_limit: usize,
) -> Result<Content, String> {
    let mut frames = Vec::new();
    let mut memory = 0usize;
    for frame in decoded {
        if frames.len() == frame_limit {
            return Err(format!(
                "GIF exceeds the {frame_limit}-frame animation limit."
            ));
        }
        let frame = frame.map_err(|error| format!("Cannot decode GIF frame: {error}"))?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        let delay = Duration::from_secs_f64(
            (f64::from(numerator) / f64::from(denominator) / 1000.0).max(0.02),
        );
        let buffer = frame.into_buffer();
        let buffer = if buffer.width() > 2048 || buffer.height() > 2048 {
            image::DynamicImage::ImageRgba8(buffer)
                .thumbnail(2048, 2048)
                .into_rgba8()
        } else {
            buffer
        };
        memory = memory
            .checked_add(buffer.len())
            .ok_or("GIF memory size overflow.")?;
        if memory > memory_limit {
            return Err("GIF exceeds the 128 MiB animation memory limit.".into());
        }
        frames.push(AnimationFrame {
            size: [buffer.width() as usize, buffer.height() as usize],
            rgba: buffer.into_raw(),
            delay,
        });
    }
    if frames.is_empty() {
        return Err("GIF contains no frames.".into());
    }
    Ok(Content::Animation(frames))
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "bmp"
                    | "ico"
                    | "tif"
                    | "tiff"
                    | "webp"
                    | "pnm"
                    | "pbm"
                    | "pgm"
                    | "ppm"
                    | "pam"
                    | "tga"
                    | "qoi"
            )
        })
}

fn load_image(path: &Path) -> Result<Content, String> {
    let mut reader = image::ImageReader::open(path)
        .map_err(|error| format!("Cannot open image: {error}"))?
        .with_guessed_format()
        .map_err(|error| format!("Cannot identify image: {error}"))?;
    reader.limits(image_limits());
    let decoded = reader
        .decode()
        .map_err(|error| format!("Cannot decode image: {error}"))?;
    let original = [decoded.width(), decoded.height()];
    let thumbnail = if original[0] > 4096 || original[1] > 4096 {
        decoded.thumbnail(4096, 4096)
    } else {
        decoded
    }
    .into_rgba8();
    let size = [thumbnail.width() as usize, thumbnail.height() as usize];
    let image = eframe::egui::ColorImage::from_rgba_unmultiplied(size, thumbnail.as_raw());
    Ok(Content::Image {
        size,
        pixels: image.pixels,
    })
}

fn load_text(path: &Path) -> Result<Option<Content>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| format!("Cannot open file: {error}"))?
        .take((TEXT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot read file: {error}"))?;
    Ok(classify_text(bytes))
}

fn classify_text(mut bytes: Vec<u8>) -> Option<Content> {
    let truncated = bytes.len() > TEXT_LIMIT;
    bytes.truncate(TEXT_LIMIT);
    if bytes
        .iter()
        .any(|byte| *byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r' | 0x0c))
    {
        return None;
    }
    if let Err(error) = std::str::from_utf8(&bytes) {
        if truncated && error.error_len().is_none() {
            bytes.truncate(error.valid_up_to());
        } else {
            return None;
        }
    }
    // The only accepted decoding error above is an incomplete final character.
    let text = String::from_utf8(bytes).expect("UTF-8 checked above");
    Some(Content::Text { text, truncated })
}

fn load_directory(path: &Path) -> Result<Content, String> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| format!("Cannot open folder: {error}"))?
        .take(DIRECTORY_LIMIT + 1)
        .map(|entry| {
            let entry = entry.map_err(|error| format!("Cannot read folder entry: {error}"))?;
            let is_dir = entry
                .file_type()
                .map_err(|error| format!("Cannot inspect folder entry: {error}"))?
                .is_dir();
            Ok(format!(
                "{}{}",
                entry.file_name().to_string_lossy(),
                if is_dir {
                    std::path::MAIN_SEPARATOR_STR
                } else {
                    ""
                }
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let truncated = entries.len() > DIRECTORY_LIMIT;
    entries.truncate(DIRECTORY_LIMIT);
    entries.sort_by_key(|name| name.to_lowercase());
    Ok(Content::Directory { entries, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "hop-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn text_limit_preserves_utf8_at_boundary() {
        let mut bytes = vec![b'a'; TEXT_LIMIT - 1];
        bytes.extend_from_slice("€tail".as_bytes());
        let Some(Content::Text { text, truncated }) = classify_text(bytes) else {
            panic!("Expected text");
        };
        assert!(truncated);
        assert_eq!(text.len(), TEXT_LIMIT - 1);
    }

    #[test]
    fn binary_and_invalid_utf8_use_metadata() {
        for bytes in [vec![0, 1, 2], vec![0xff, 0xfe], vec![0x61, 0xe2]] {
            assert!(classify_text(bytes).is_none());
        }
        assert!(matches!(
            classify_text(Vec::new()),
            Some(Content::Text {
                truncated: false,
                ..
            })
        ));
    }

    #[test]
    fn loads_text_and_reports_missing_paths() {
        let dir = TestDirectory::new();
        let path = dir.0.join("hello.txt");
        fs::write(&path, "Hello\nworld").unwrap();
        let preview = load(&path).unwrap();
        assert!(
            matches!(preview.content, Content::Text { text, truncated: false } if text == "Hello\nworld")
        );
        assert!(load(&dir.0.join("missing")).is_err());
    }

    #[test]
    fn unsupported_documents_show_file_type_and_exact_size_even_if_ascii() {
        let dir = TestDirectory::new();
        for extension in ["PDF", "docx", "zip", "mp3"] {
            let path = dir.0.join(format!("unsupported.{extension}"));
            fs::write(&path, b"not a supported text document").unwrap();
            let Content::Unsupported {
                bytes,
                extension: actual,
                ..
            } = load(&path).unwrap().content
            else {
                panic!("Expected file information for {extension}");
            };
            assert_eq!(bytes, 29);
            assert_eq!(actual, extension.to_uppercase());
        }
    }

    #[test]
    fn unknown_binary_and_extensionless_files_show_file_information() {
        let dir = TestDirectory::new();
        for name in ["unknown.hop", "no-extension"] {
            let path = dir.0.join(name);
            fs::write(&path, [0, 1, 2, 3]).unwrap();
            let Content::Unsupported {
                bytes, extension, ..
            } = load(&path).unwrap().content
            else {
                panic!("Expected file information");
            };
            assert_eq!(bytes, 4);
            assert_eq!(extension, if name.contains('.') { "HOP" } else { "FILE" });
        }
        let path = dir.0.join("empty.pdf");
        fs::write(&path, []).unwrap();
        assert!(matches!(
            load(&path).unwrap().content,
            Content::Unsupported { bytes: 0, .. }
        ));
    }

    #[test]
    fn directory_listing_is_bounded() {
        let dir = TestDirectory::new();
        for index in 0..DIRECTORY_LIMIT + 3 {
            fs::write(dir.0.join(format!("{index:04}.txt")), "").unwrap();
        }
        let Content::Directory { entries, truncated } = load(&dir.0).unwrap().content else {
            panic!("Expected directory");
        };
        assert_eq!(entries.len(), DIRECTORY_LIMIT);
        assert!(truncated);
        assert!(entries.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn decodes_images_and_reports_corrupt_images() {
        let dir = TestDirectory::new();
        let path = dir.0.join("test.PNG");
        image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .unwrap();
        let Content::Image { size, pixels } = load(&path).unwrap().content else {
            panic!("Expected image");
        };
        assert_eq!(size, [3, 2]);
        assert_eq!(pixels, vec![eframe::egui::Color32::from_rgb(10, 20, 30); 6]);
        fs::write(&path, "broken").unwrap();
        assert!(load(&path).is_err());
    }

    #[test]
    fn still_images_are_texture_ready_with_transparency_preserved() {
        let dir = TestDirectory::new();
        let path = dir.0.join("transparent.png");
        let rgba = [10, 20, 30, 255, 100, 150, 200, 128, 255, 120, 60, 0];
        image::RgbaImage::from_raw(3, 1, rgba.to_vec())
            .unwrap()
            .save(&path)
            .unwrap();
        let Content::Image { size, pixels } = load(&path).unwrap().content else {
            panic!("Expected image");
        };
        assert_eq!(size, [3, 1]);
        assert_eq!(
            pixels,
            eframe::egui::ColorImage::from_rgba_unmultiplied(size, &rgba).pixels
        );
    }

    #[test]
    fn decodes_png_bmp_and_both_tiff_extensions() {
        let dir = TestDirectory::new();
        for extension in ["PNG", "bmp", "tif", "tiff"] {
            let path = dir.0.join(format!("test.{extension}"));
            image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]))
                .save(&path)
                .unwrap();
            let Content::Image { size, pixels } = load(&path).unwrap().content else {
                panic!("Expected image for {extension}");
            };
            assert_eq!(size, [3, 2]);
            assert_eq!(
                pixels,
                vec![eframe::egui::Color32::from_rgb(10, 20, 30); 6],
                "{extension}"
            );
        }
    }

    fn gif_frame(color: [u8; 4], delay: u32) -> image::Frame {
        image::Frame::from_parts(
            image::RgbaImage::from_pixel(2, 1, image::Rgba(color)),
            0,
            0,
            image::Delay::from_numer_denom_ms(delay, 1),
        )
    }

    #[test]
    fn decodes_gif_frames_colors_and_timing() {
        let dir = TestDirectory::new();
        let path = dir.0.join("animated.GIF");
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(File::create(&path).unwrap());
            encoder
                .encode_frames([
                    gif_frame([255, 0, 0, 255], 40),
                    gif_frame([0, 255, 0, 255], 120),
                ])
                .unwrap();
        }
        let Content::Animation(frames) = load(&path).unwrap().content else {
            panic!("Expected GIF animation");
        };
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].size, [2, 1]);
        assert_eq!(&frames[0].rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(&frames[1].rgba[..4], &[0, 255, 0, 255]);
        assert_eq!(frames[0].delay, Duration::from_millis(40));
        assert_eq!(frames[1].delay, Duration::from_millis(120));
        fs::write(&path, "broken GIF").unwrap();
        assert!(load(&path).is_err());
    }

    #[test]
    fn gif_frame_and_memory_limits_are_enforced() {
        let frames = || {
            [
                Ok(gif_frame([255, 0, 0, 255], 0)),
                Ok(gif_frame([0, 255, 0, 255], 0)),
            ]
        };
        assert!(
            collect_gif_frames(frames(), 1, 1024)
                .err()
                .unwrap()
                .contains("frame")
        );
        assert!(
            collect_gif_frames(frames(), 10, 8)
                .err()
                .unwrap()
                .contains("memory")
        );
        let Content::Animation(decoded) = collect_gif_frames(frames(), 2, 16).unwrap() else {
            panic!("Expected animation");
        };
        assert_eq!(decoded[0].delay, Duration::from_millis(20));
        assert!(collect_gif_frames([], 10, 1024).is_err());
    }

    #[test]
    fn routes_video_extensions_without_reading_them_as_text() {
        let dir = TestDirectory::new();
        for extension in ["MP4", "mkv", "webm", "mov", "avi", "wmv", "m4v", "ts"] {
            let path = dir.0.join(format!("video.{extension}"));
            fs::write(&path, [0, 1, 2]).unwrap();
            assert!(
                matches!(load(&path).unwrap().content, Content::Video),
                "{extension}"
            );
        }
    }

    #[test]
    fn exact_text_limit_is_not_marked_truncated() {
        assert!(matches!(
            classify_text(vec![b'a'; TEXT_LIMIT]),
            Some(Content::Text { text, truncated: false }) if text.len() == TEXT_LIMIT
        ));
    }

    #[test]
    fn image_dimensions_are_limited_and_textures_are_downscaled() {
        let dir = TestDirectory::new();
        let path = dir.0.join("wide.png");
        image::RgbaImage::new(16385, 1).save(&path).unwrap();
        assert!(load(&path).is_err());
        image::RgbaImage::new(5000, 1).save(&path).unwrap();
        assert!(matches!(
            load(&path).unwrap().content,
            Content::Image {
                size: [4096, 1],
                ..
            }
        ));
    }

    #[test]
    fn oversized_images_are_not_decoded() {
        let dir = TestDirectory::new();
        let path = dir.0.join("large.png");
        File::create(&path)
            .unwrap()
            .set_len(IMAGE_FILE_LIMIT + 1)
            .unwrap();
        assert!(matches!(
            load(&path).unwrap().content,
            Content::Unsupported { .. }
        ));
    }
}
