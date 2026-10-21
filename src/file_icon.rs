use std::path::Path;

pub struct FileIcon {
    pub rgba: Vec<u8>,
    pub size: [usize; 2],
}

pub fn load(path: &Path) -> Option<FileIcon> {
    #[cfg(windows)]
    {
        match native::load(path) {
            Ok(icon) => Some(icon),
            Err(error) => {
                eprintln!(
                    "Hop: Cannot load file-type icon for {}: {error}",
                    path.display()
                );
                None
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
        DeleteDC, DeleteObject, GdiFlush, HBITMAP, HDC, HGDIOBJ, SelectObject,
    };
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    use windows::Win32::UI::Shell::{
        SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{DI_NORMAL, DestroyIcon, DrawIconEx, HICON};
    use windows::core::PCWSTR;

    const SIZE: usize = 64;
    static ICON_LOOKUP: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct OwnedIcon(HICON);

    impl Drop for OwnedIcon {
        fn drop(&mut self) {
            if let Err(error) = unsafe { DestroyIcon(self.0) } {
                eprintln!("Hop: Cannot release file icon: {error}");
            }
        }
    }

    struct Surface {
        dc: HDC,
        bitmap: HBITMAP,
        previous: HGDIOBJ,
    }

    impl Drop for Surface {
        fn drop(&mut self) {
            unsafe {
                if !self.previous.0.is_null() {
                    SelectObject(self.dc, self.previous);
                }
                if !self.bitmap.0.is_null() && !DeleteObject(self.bitmap.into()).as_bool() {
                    eprintln!("Hop: Cannot release icon bitmap.");
                }
                if !DeleteDC(self.dc).as_bool() {
                    eprintln!("Hop: Cannot release icon drawing context.");
                }
            }
        }
    }

    pub fn load(path: &Path) -> Result<FileIcon, String> {
        // Concurrent first lookups can return success with a null shell icon.
        let _lookup = ICON_LOOKUP
            .lock()
            .map_err(|error| format!("Shell icon lookup lock failed: {error}"))?;
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|error| format!("Cannot initialize shell icon lookup: {error}"))?;
        let _apartment = crate::windows::ComApartment;
        let mut name = std::ffi::OsString::from("file");
        if let Some(extension) = path.extension() {
            name.push(".");
            name.push(extension);
        }
        // Query the association, not thumbnails or per-file executable resources.
        let lookup = std::env::temp_dir().join(name);
        let wide: Vec<u16> = lookup.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut info = SHFILEINFOW::default();
        let result = unsafe {
            SHGetFileInfoW(
                PCWSTR(wide.as_ptr()),
                FILE_ATTRIBUTE_NORMAL,
                Some(&mut info),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES,
            )
        };
        if result == 0 || info.hIcon.0.is_null() {
            return Err(format!(
                "Windows did not return a file-type icon (result={result}, handle={:?}, error={}).",
                info.hIcon,
                std::io::Error::last_os_error()
            ));
        }
        let icon = OwnedIcon(info.hIcon);
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.0.is_null() {
            return Err("Cannot create icon drawing context.".into());
        }
        let mut surface = Surface {
            dc,
            bitmap: HBITMAP::default(),
            previous: HGDIOBJ::default(),
        };
        let bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: SIZE as i32,
                biHeight: -(SIZE as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        surface.bitmap =
            unsafe { CreateDIBSection(Some(dc), &bitmap_info, DIB_RGB_COLORS, &mut bits, None, 0) }
                .map_err(|error| error.to_string())?;
        surface.previous = unsafe { SelectObject(dc, surface.bitmap.into()) };
        if surface.previous.0.is_null() || surface.previous.0 as isize == -1 || bits.is_null() {
            surface.previous = HGDIOBJ::default();
            return Err("Cannot select icon bitmap.".into());
        }
        // Two backgrounds preserve transparency for both alpha and legacy mask icons.
        let mut backgrounds = Vec::with_capacity(2);
        for background in [0, 255] {
            unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), SIZE * SIZE * 4) }
                .fill(background);
            unsafe {
                DrawIconEx(
                    dc,
                    0,
                    0,
                    icon.0,
                    SIZE as i32,
                    SIZE as i32,
                    0,
                    None,
                    DI_NORMAL,
                )
            }
            .map_err(|error| format!("Cannot draw file icon: {error}"))?;
            if !unsafe { GdiFlush() }.as_bool() {
                return Err("Cannot finish drawing file icon.".into());
            }
            backgrounds.push(
                unsafe { std::slice::from_raw_parts(bits.cast::<u8>(), SIZE * SIZE * 4) }.to_vec(),
            );
        }
        let mut rgba = Vec::with_capacity(SIZE * SIZE * 4);
        for (black, white) in backgrounds[0]
            .chunks_exact(4)
            .zip(backgrounds[1].chunks_exact(4))
        {
            let alpha = 255
                - (0..3)
                    .map(|channel| white[channel].saturating_sub(black[channel]))
                    .max()
                    .unwrap_or(0);
            for channel in [2, 1, 0] {
                rgba.push(if alpha == 0 {
                    0
                } else {
                    (u32::from(black[channel]) * 255 / u32::from(alpha)).min(255) as u8
                });
            }
            rgba.push(alpha);
        }
        Ok(FileIcon {
            rgba,
            size: [SIZE, SIZE],
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn loads_shell_icons_for_registered_and_unknown_extensions() {
            for path in [
                "example.pdf",
                "example.zip",
                "example.hop-unknown",
                "no-extension",
            ] {
                let icon = load(Path::new(path)).unwrap_or_else(|error| panic!("{path}: {error}"));
                assert_eq!(icon.size, [64, 64]);
                assert_eq!(icon.rgba.len(), 64 * 64 * 4);
                assert!(icon.rgba.chunks_exact(4).any(|pixel| pixel[3] > 0));
            }
        }
    }
}
