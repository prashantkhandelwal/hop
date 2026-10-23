use eframe::egui;
use hayro::hayro_syntax::Pdf;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Duration;

const FILE_LIMIT: u64 = 64 * 1024 * 1024;
const RENDER_AXIS_LIMIT: f32 = 2400.0;

struct RenderedPage {
    image: egui::ColorImage,
    points: egui::Vec2,
}

pub struct PdfPreview {
    page_count: usize,
    page: usize,
    requests: Sender<usize>,
    results: Receiver<(usize, Result<RenderedPage, String>)>,
    image: Option<RenderedPage>,
    points: egui::Vec2,
    texture: Option<egui::TextureHandle>,
    loading: bool,
    error: Option<String>,
    zoom: Option<f32>,
    last_scroll_offset: Option<egui::Vec2>,
}

impl PdfPreview {
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|error| format!("Cannot open PDF: {error}"))?;
        if file
            .metadata()
            .map_err(|error| format!("Cannot inspect PDF: {error}"))?
            .len()
            > FILE_LIMIT
        {
            return Err("PDF exceeds the 64 MiB file limit.".into());
        }
        let mut bytes = Vec::new();
        file.take(FILE_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Cannot read PDF: {error}"))?;
        if bytes.len() as u64 > FILE_LIMIT {
            return Err("PDF exceeds the 64 MiB file limit.".into());
        }
        let pdf = Pdf::new(bytes).map_err(|error| {
            format!("Cannot open PDF (invalid or password-protected): {error:?}")
        })?;
        let page_count = pdf.pages().len();
        if page_count == 0 {
            return Err("PDF contains no pages.".into());
        }
        let image = render_page(&pdf, 0)?;
        let (requests, pending) = mpsc::channel();
        let (sender, results) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("pdf-preview".into())
            .spawn(move || {
                while let Ok(next) = pending.recv() {
                    let page = pending.try_iter().last().unwrap_or(next);
                    if sender.send((page, render_page(&pdf, page))).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| format!("Cannot start PDF renderer: {error}"))?;
        Ok(Self {
            page_count,
            page: 0,
            requests,
            results,
            image: Some(image),
            points: egui::Vec2::ZERO,
            texture: None,
            loading: false,
            error: None,
            zoom: None,
            last_scroll_offset: None,
        })
    }

    fn request_page(&mut self, page: usize) {
        if page == self.page {
            return;
        }
        if page >= self.page_count {
            self.fail("PDF page is out of range.".into());
            return;
        }
        self.page = page;
        self.image = None;
        self.texture = None;
        self.error = None;
        self.loading = true;
        if let Err(error) = self.requests.send(page) {
            self.fail(format!("Cannot request PDF page: {error}"));
        }
    }

    fn fail(&mut self, error: String) {
        eprintln!("Hop: {error}");
        self.error = Some(error);
        self.loading = false;
    }

    fn receive(&mut self) {
        loop {
            match self.results.try_recv() {
                Ok((page, result)) if page == self.page => {
                    self.loading = false;
                    match result {
                        Ok(image) => self.image = Some(image),
                        Err(error) => self.fail(error),
                    }
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.loading {
                        self.fail("PDF renderer stopped unexpectedly.".into());
                    }
                    break;
                }
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.receive();
        self.handle_keyboard(ui);
        egui::Frame::NONE.inner_margin(8).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(self.page > 0, egui::Button::new("Previous"))
                    .clicked()
                {
                    self.request_page(self.page - 1);
                }
                let mut page = self.page + 1;
                if ui
                    .add(
                        egui::DragValue::new(&mut page)
                            .range(1..=self.page_count)
                            .prefix("Page "),
                    )
                    .changed()
                {
                    self.request_page(page - 1);
                }
                ui.label(format!("of {}", self.page_count));
                if ui
                    .add_enabled(self.page + 1 < self.page_count, egui::Button::new("Next"))
                    .clicked()
                {
                    self.request_page(self.page + 1);
                }
                if ui.button("Fit page").clicked() {
                    self.zoom = None;
                }
                let mut percent = self.zoom.unwrap_or(1.0) * 100.0;
                if ui
                    .add(
                        egui::DragValue::new(&mut percent)
                            .range(25.0..=300.0)
                            .suffix("%"),
                    )
                    .on_hover_text("Zoom (100% = PDF point size)")
                    .changed()
                {
                    self.zoom = Some(percent / 100.0);
                }
            });
        });
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            return;
        }
        if self.loading {
            let rect = egui::Rect::from_center_size(
                ui.available_rect_before_wrap().center(),
                egui::Vec2::splat(24.0),
            );
            ui.put(rect, egui::Spinner::new());
            ui.ctx().request_repaint_after(Duration::from_millis(30));
            return;
        }
        if let Some(image) = self.image.take() {
            self.points = image.points;
            self.texture = Some(ui.ctx().load_texture(
                "pdf-page",
                image.image,
                egui::TextureOptions::LINEAR,
            ));
        }
        if let Some(texture) = &self.texture {
            let available =
                (ui.available_size() - egui::Vec2::splat(16.0)).max(egui::Vec2::splat(1.0));
            let size = texture.size_vec2();
            let size = self.zoom.map_or_else(
                || size * (available.x / size.x).min(available.y / size.y),
                |zoom| self.points * zoom,
            );

            let mut scroll = egui::ScrollArea::both()
                .id_salt(("pdf-page", self.page))
                .auto_shrink([false, false]);
            if self.zoom.is_some() {
                scroll = scroll
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible);
            }
            let scroll_response = scroll.show(ui, |ui| {
                let (rect, _) = ui.allocate_exact_size(size.max(available), egui::Sense::hover());
                egui::Image::new(texture)
                    .paint_at(ui, egui::Rect::from_center_size(rect.center(), size));
            });

            // Handle scroll navigation
            if self.zoom.is_none() {
                // Fit-to-page mode: scroll always navigates
                let scroll_delta = ui.ctx().input(|input| input.raw_scroll_delta.y);
                let has_ctrl = ui.ctx().input(|input| input.modifiers.ctrl);
                
                if scroll_delta != 0.0 && !has_ctrl {
                    if scroll_delta > 0.0 && self.page > 0 {
                        self.request_page(self.page - 1);
                    } else if scroll_delta < 0.0 && self.page + 1 < self.page_count {
                        self.request_page(self.page + 1);
                    }
                }
                self.last_scroll_offset = None;
            } else {
                // Zoomed mode: navigate at scroll edges by detecting if scroll didn't move content
                let offset = scroll_response.state.offset;
                let vertical_scrollable = size.y > available.y;
                
                if vertical_scrollable {
                    let scroll_delta = ui.ctx().input(|input| input.raw_scroll_delta.y);
                    let has_ctrl = ui.ctx().input(|input| input.modifiers.ctrl);
                    
                    if scroll_delta != 0.0 && !has_ctrl {
                        // Check if offset actually changed compared to last frame
                        let offset_changed = if let Some(last_offset) = self.last_scroll_offset {
                            (last_offset.y - offset.y).abs() > 0.1
                        } else {
                            true // First frame, assume it could have changed
                        };
                        
                        // If scroll was applied but offset didn't actually change, we're at edge
                        if !offset_changed {
                            if scroll_delta > 0.0 && self.page > 0 {
                                self.request_page(self.page - 1);
                            } else if scroll_delta < 0.0 && self.page + 1 < self.page_count {
                                self.request_page(self.page + 1);
                            }
                        }
                    }
                    self.last_scroll_offset = Some(offset);
                } else {
                    self.last_scroll_offset = None;
                }
            }
        }
    }

    fn handle_keyboard(&mut self, ui: &egui::Ui) {
        let ctx = ui.ctx();
        let mut handle_key = |key, action: fn(&mut Self)| {
            ctx.input_mut(|input| {
                if input.key_pressed(key) {
                    action(self);
                }
            });
        };
        handle_key(egui::Key::ArrowUp, |this| {
            if this.page > 0 {
                this.request_page(this.page - 1);
            }
        });
        handle_key(egui::Key::ArrowDown, |this| {
            if this.page + 1 < this.page_count {
                this.request_page(this.page + 1);
            }
        });
        handle_key(egui::Key::PageUp, |this| {
            if this.page > 0 {
                this.request_page(this.page - 1);
            }
        });
        handle_key(egui::Key::PageDown, |this| {
            if this.page + 1 < this.page_count {
                this.request_page(this.page + 1);
            }
        });
        handle_key(egui::Key::Home, |this| {
            if this.page > 0 {
                this.request_page(0);
            }
        });
        handle_key(egui::Key::End, |this| {
            if this.page + 1 < this.page_count {
                this.request_page(this.page_count - 1);
            }
        });
        ctx.input_mut(|input| {
            let scroll = input.raw_scroll_delta.y;
            if scroll != 0.0 && input.modifiers.ctrl {
                let zoom = self.zoom.unwrap_or(1.0);
                let new_zoom = if scroll > 0.0 {
                    (zoom * 1.1).min(3.0)
                } else {
                    (zoom / 1.1).max(0.25)
                };
                self.zoom = if new_zoom == 1.0 {
                    None
                } else {
                    Some(new_zoom)
                };
            }
        });
    }
}

fn render_page(pdf: &Pdf, index: usize) -> Result<RenderedPage, String> {
    let page = pdf.pages().get(index).ok_or("PDF page is out of range.")?;
    let (width, height) = page.render_dimensions();
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err("PDF page has invalid dimensions.".into());
    }
    let scale = 2.0_f32.min(RENDER_AXIS_LIMIT / width.max(height));
    if width * scale < 1.0 || height * scale < 1.0 {
        return Err("PDF page is too narrow or too small to render.".into());
    }
    let pixmap = hayro::render(
        page,
        &hayro::RenderCache::new(),
        &Default::default(),
        &Default::default(),
        &hayro::PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: hayro::vello_cpu::color::AlphaColor::WHITE,
        },
    );
    Ok(RenderedPage {
        image: egui::ColorImage::from_rgba_premultiplied(
            [pixmap.width() as usize, pixmap.height() as usize],
            pixmap.data_as_u8_slice(),
        ),
        points: egui::vec2(width, height),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(width: u32, height: u32) -> Vec<u8> {
        let stream = "1 0 0 rg 0 0 20 20 re f";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_owned(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Resources << >> /Contents 4 0 R >>"
            ),
            format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            ),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 60 80] /Resources << >> >>".to_owned(),
        ];
        let mut pdf = "%PDF-1.4\n".to_owned();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
        }
        let xref = pdf.len();
        pdf.push_str("xref\n0 6\n0000000000 65535 f \n");
        for offset in offsets {
            pdf.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.push_str(&format!(
            "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        ));
        pdf.into_bytes()
    }

    fn preview() -> PdfPreview {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.PDF");
        std::fs::write(&path, document(100, 120)).unwrap();
        let crate::preview::Content::Pdf(preview) = crate::preview::load(&path).unwrap().content
        else {
            panic!("Expected PDF preview");
        };
        *preview
    }

    #[test]
    fn renders_pdf_shapes_on_white_and_bounds_large_pages() {
        let pdf = Pdf::new(document(100, 120)).unwrap();
        let page = render_page(&pdf, 0).unwrap();
        assert_eq!(page.image.size, [200, 240]);
        assert_eq!(page.points, egui::vec2(100.0, 120.0));
        assert_eq!(page.image[(10, 230)], egui::Color32::RED);
        assert_eq!(page.image[(100, 100)], egui::Color32::WHITE);
        assert!(render_page(&pdf, 2).is_err());
        let large = Pdf::new(document(10000, 20000)).unwrap();
        let page = render_page(&large, 0).unwrap();
        assert_eq!(page.image.size, [1200, 2400]);
        assert_eq!(page.points, egui::vec2(10000.0, 20000.0));
    }

    #[test]
    fn navigates_on_worker_and_discards_stale_results() {
        let mut preview = preview();
        assert_eq!(preview.page_count, 2);
        preview.request_page(1);
        assert!(preview.loading);
        assert!(preview.image.is_none());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while preview.loading {
            preview.receive();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(preview.error.is_none());
        assert_eq!(preview.image.as_ref().unwrap().image.size, [120, 160]);
        preview.request_page(0);
        preview.request_page(1);
        while preview.loading {
            preview.receive();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(preview.image.as_ref().unwrap().image.size, [120, 160]);
    }

    #[test]
    fn rejects_invalid_and_oversized_pdfs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid.pdf");
        for bytes in [b"".as_slice(), b"not a PDF".as_slice()] {
            std::fs::write(&path, bytes).unwrap();
            assert!(
                PdfPreview::load(&path)
                    .err()
                    .unwrap()
                    .contains("Cannot open PDF")
            );
        }
        File::create(&path)
            .unwrap()
            .set_len(FILE_LIMIT + 1)
            .unwrap();
        assert!(PdfPreview::load(&path).err().unwrap().contains("64 MiB"));
    }

    #[test]
    fn shows_controls_uploads_texture_and_preserves_zoom() {
        let mut preview = preview();
        let ctx = egui::Context::default();
        let frame = |preview: &mut PdfPreview| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 640.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| preview.show(ui));
                },
            )
        };
        let output = frame(&mut preview);
        assert!(preview.texture.is_some());
        assert!(preview.image.is_none());
        assert!(output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Next")
        }));
        preview.zoom = Some(3.0);
        let output = frame(&mut preview);
        let texture = preview.texture.as_ref().unwrap().id();
        let page = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect)
                    if rect
                        .brush
                        .as_ref()
                        .is_some_and(|brush| brush.fill_texture_id == texture) =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .expect("Expected rendered PDF page");
        assert_eq!(page.size(), egui::vec2(300.0, 360.0));
        assert_eq!(preview.zoom, Some(3.0));
        preview.request_page(1);
        assert!(preview.texture.is_none());
        assert_eq!(preview.zoom, Some(3.0));
    }

    #[test]
    fn renderer_failure_is_visible_and_leaving_document_closes_worker() {
        let mut preview = preview();
        let (requests, pending) = mpsc::channel();
        preview.requests = requests;
        let (sender, results) = mpsc::channel();
        preview.results = results;
        drop(sender);
        preview.request_page(1);
        preview.receive();
        assert!(!preview.loading);
        assert_eq!(
            preview.error.as_deref(),
            Some("PDF renderer stopped unexpectedly.")
        );
        assert_eq!(pending.recv().unwrap(), 1);
        drop(preview);
        assert!(pending.recv_timeout(Duration::from_secs(1)).is_err());
    }

    #[test]
    fn scroll_wheel_navigates_pages_in_fit_to_page_mode() {
        let mut preview = preview();
        assert_eq!(preview.page, 0);
        assert_eq!(preview.zoom, None);
        
        // Wait for page to render
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while preview.loading {
            preview.receive();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }

        let ctx = egui::Context::default();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 640.0),
                )),
                ..Default::default()
            },
            |_| {
                egui::CentralPanel::default().show(&ctx, |ui| {
                    ui.ctx().input_mut(|input| {
                        input.raw_scroll_delta = egui::vec2(0.0, -1.0);
                    });
                    preview.show(ui);
                });
            },
        );
        assert_eq!(preview.page, 1, "Scroll down should navigate to next page");
        
        // Wait for page 1 to render
        while preview.loading {
            preview.receive();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }

        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 640.0),
                )),
                ..Default::default()
            },
            |_| {
                egui::CentralPanel::default().show(&ctx, |ui| {
                    ui.ctx().input_mut(|input| {
                        input.raw_scroll_delta = egui::vec2(0.0, 1.0);
                    });
                    preview.show(ui);
                });
            },
        );
        assert_eq!(preview.page, 0, "Scroll up should navigate to previous page");
    }

    #[test]
    fn scroll_wheel_does_not_navigate_when_zoomed() {
        let mut preview = preview();
        
        // Wait for page to render first
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while preview.loading {
            preview.receive();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        
        preview.zoom = Some(2.0);
        assert_eq!(preview.page, 0);
        
        let ctx = egui::Context::default();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 640.0),
                )),
                ..Default::default()
            },
            |_| {
                egui::CentralPanel::default().show(&ctx, |ui| {
                    ui.ctx().input_mut(|input| {
                        input.raw_scroll_delta = egui::vec2(0.0, -1.0);
                    });
                    preview.show(ui);
                });
            },
        );
        assert_eq!(
            preview.page, 0,
            "Scroll wheel should not navigate when zoomed and not at bottom (ScrollArea handles scrolling)"
        );
    }
}
