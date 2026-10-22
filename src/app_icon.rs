pub fn load() -> Result<eframe::egui::IconData, String> {
    let image = image::load_from_memory_with_format(
        include_bytes!("../assets/hop.png"),
        image::ImageFormat::Png,
    )
    .map_err(|error| format!("Cannot decode the Hop application icon: {error}"))?
    .into_rgba8();
    Ok(eframe::egui::IconData {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_icon_is_rgba_with_transparent_corners() {
        let icon = load().unwrap();
        assert_eq!([icon.width, icon.height], [512, 512]);
        assert_eq!(icon.rgba.len(), 512 * 512 * 4);
        assert_eq!(icon.rgba[3], 0);
        assert!(icon.rgba.chunks_exact(4).any(|pixel| pixel[3] == 255));
    }

    #[test]
    fn windows_icon_contains_sizes_for_tray_and_explorer() {
        let bytes = include_bytes!("../assets/hop.ico");
        assert_eq!(&bytes[..4], &[0, 0, 1, 0]);
        let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
        let mut sizes = Vec::new();
        for entry in bytes[6..6 + count * 16].chunks_exact(16) {
            let size = if entry[0] == 0 {
                256
            } else {
                u32::from(entry[0])
            };
            let length = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
            let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
            let image = image::load_from_memory_with_format(
                &bytes[offset..offset + length],
                image::ImageFormat::Png,
            )
            .unwrap();
            assert_eq!([image.width(), image.height()], [size, size]);
            sizes.push(size);
        }
        assert_eq!(sizes, [16, 20, 24, 32, 48, 64, 128, 256]);
    }
}
