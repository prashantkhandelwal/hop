use eframe::egui;
use glam::{DMat4, DVec3, Mat3, Vec3};
use roxmltree::Node;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::time::Duration;

const FILE_LIMIT: u64 = 64 * 1024 * 1024;
const XML_LIMIT: u64 = 32 * 1024 * 1024;
const TRIANGLE_LIMIT: usize = 200_000;
const VERTEX_LIMIT: usize = TRIANGLE_LIMIT * 3;
const CORE_NS: &str = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02";
const MODEL_REL: &str = "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel";
const BACKGROUND: egui::Color32 = egui::Color32::from_gray(240);

type Triangle = [Vec3; 3];
type RawTriangle = [DVec3; 3];

#[derive(Clone, Copy, PartialEq)]
struct View {
    size: [usize; 2],
    yaw: f32,
    pitch: f32,
    zoom: f32,
}

impl View {
    fn scroll(&mut self, delta: f32) {
        self.zoom = (self.zoom * (delta * 0.002).exp()).clamp(0.1, 10.0);
    }

    fn drag(&mut self, delta: egui::Vec2) {
        self.yaw = (self.yaw + delta.x * 0.01).rem_euclid(std::f32::consts::TAU);
        self.pitch = (self.pitch + delta.y * 0.01).rem_euclid(std::f32::consts::TAU);
    }
}

pub struct ModelPreview {
    requests: SyncSender<View>,
    results: Receiver<(View, egui::ColorImage)>,
    view: View,
    submitted: Option<View>,
    displayed: Option<View>,
    texture: Option<egui::TextureHandle>,
    error: Option<String>,
}

impl ModelPreview {
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|error| format!("Cannot open 3D model: {error}"))?;
        if file
            .metadata()
            .map_err(|error| format!("Cannot inspect 3D model: {error}"))?
            .len()
            > FILE_LIMIT
        {
            return Err("3D model exceeds the 64 MiB file limit.".into());
        }
        let mut bytes = Vec::new();
        file.take(FILE_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Cannot read 3D model: {error}"))?;
        if bytes.len() as u64 > FILE_LIMIT {
            return Err("3D model exceeds the 64 MiB file limit.".into());
        }
        let triangles = if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("stl"))
        {
            load_stl(&bytes)?
        } else {
            load_3mf(Cursor::new(bytes))?
        };
        let triangles = normalize(triangles)?;
        let (requests, pending) = mpsc::sync_channel::<View>(1);
        let (sender, results) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("model-preview".into())
            .spawn(move || {
                while let Ok(view) = pending.recv() {
                    if sender.send((view, render(&triangles, view))).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| format!("Cannot start 3D renderer: {error}"))?;
        Ok(Self {
            requests,
            results,
            view: View {
                size: [1, 1],
                yaw: -0.6,
                pitch: -1.1,
                zoom: 1.0,
            },
            submitted: None,
            displayed: None,
            texture: None,
            error: None,
        })
    }

    fn fail(&mut self, error: &str) {
        eprintln!("Hop: {error}");
        self.error = Some(error.into());
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            return;
        }
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::drag());
        let pixels = rect.size() * ui.ctx().pixels_per_point();
        let scale = (800.0 / pixels.max_elem().max(1.0)).min(1.0);
        self.view.size = [
            (pixels.x * scale).round().max(1.0) as usize,
            (pixels.y * scale).round().max(1.0) as usize,
        ];
        if response.hovered() {
            self.view.scroll(ui.input(|input| input.raw_scroll_delta.y));
        }
        if response.dragged_by(egui::PointerButton::Primary) {
            self.view.drag(ui.input(|input| input.pointer.delta()));
        }
        loop {
            match self.results.try_recv() {
                Ok((view, image)) => {
                    if let Some(texture) = &mut self.texture {
                        texture.set(image, egui::TextureOptions::LINEAR);
                    } else {
                        self.texture = Some(ui.ctx().load_texture(
                            "model-preview",
                            image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                    self.displayed = Some(view);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.fail("3D renderer stopped unexpectedly.");
                    return;
                }
            }
        }
        if self.submitted != Some(self.view) {
            match self.requests.try_send(self.view) {
                Ok(()) => self.submitted = Some(self.view),
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => {
                    self.fail("Cannot request a 3D render.");
                    return;
                }
            }
        }
        if let Some(texture) = &self.texture {
            egui::Image::new(texture).paint_at(ui, rect);
        } else {
            ui.put(
                egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(24.0)),
                egui::Spinner::new(),
            );
        }
        response.on_hover_text("Scroll to zoom. Left-drag to rotate.");
        if self.displayed != Some(self.view) {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
    }
}

fn load_stl(bytes: &[u8]) -> Result<Vec<RawTriangle>, String> {
    let mut cursor = Cursor::new(bytes);
    let reader = stl_io::create_stl_reader(&mut cursor)
        .map_err(|error| format!("Cannot decode STL: {error}"))?;
    let mut triangles = Vec::new();
    for face in reader {
        let face = face.map_err(|error| format!("Cannot decode STL triangle: {error}"))?;
        push_triangle(
            &mut triangles,
            face.vertices
                .map(|v| DVec3::new(f64::from(v[0]), f64::from(v[1]), f64::from(v[2]))),
        )?;
    }
    Ok(triangles)
}

fn push_triangle(triangles: &mut Vec<RawTriangle>, triangle: RawTriangle) -> Result<(), String> {
    if triangles.len() == TRIANGLE_LIMIT {
        return Err("3D model exceeds the 200,000-triangle preview limit.".into());
    }
    if triangle.iter().any(|vertex| !vertex.is_finite()) {
        return Err("3D model contains non-finite coordinates.".into());
    }
    triangles.push(triangle);
    Ok(())
}

fn read_entry<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<String, String> {
    let entry = archive
        .by_name(name)
        .map_err(|error| format!("Cannot read 3MF entry {name}: {error}"))?;
    if entry.size() > XML_LIMIT {
        return Err("3MF XML exceeds the 32 MiB preview limit.".into());
    }
    let mut bytes = Vec::new();
    entry
        .take(XML_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot decompress 3MF entry {name}: {error}"))?;
    if bytes.len() as u64 > XML_LIMIT {
        return Err("3MF XML exceeds the 32 MiB preview limit.".into());
    }
    String::from_utf8(bytes).map_err(|error| format!("3MF XML is not UTF-8: {error}"))
}

fn load_3mf<R: Read + Seek>(reader: R) -> Result<Vec<RawTriangle>, String> {
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|error| format!("Cannot open 3MF: {error}"))?;
    let relationships = read_entry(&mut archive, "_rels/.rels")?;
    let doc = roxmltree::Document::parse(&relationships)
        .map_err(|error| format!("Cannot decode 3MF relationships: {error}"))?;
    let mut models = doc.descendants().filter(|node| {
        node.has_tag_name("Relationship") && node.attribute("Type") == Some(MODEL_REL)
    });
    let model = models
        .next()
        .ok_or("3MF package has no model relationship.")?;
    if models.next().is_some() || model.attribute("TargetMode") == Some("External") {
        return Err("3MF must contain one internal root model.".into());
    }
    let target = required(model, "Target")?;
    let mut parts = Vec::new();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or("Invalid 3MF model path.")?;
            }
            part if part.contains([':', '\\']) => return Err("Invalid 3MF model path.".into()),
            part => parts.push(part),
        }
    }
    let xml = read_entry(&mut archive, &parts.join("/"))?;
    parse_model(&xml)
}

fn required<'a>(node: Node<'a, '_>, name: &str) -> Result<&'a str, String> {
    node.attribute(name)
        .ok_or_else(|| format!("3MF {} is missing {name}.", node.tag_name().name()))
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Result<Node<'a, 'input>, String> {
    node.children()
        .find(|child| child.has_tag_name((CORE_NS, name)))
        .ok_or_else(|| format!("3MF {} is missing {name}.", node.tag_name().name()))
}

fn number(node: Node<'_, '_>, name: &str) -> Result<f64, String> {
    let value: f64 = required(node, name)?
        .parse()
        .map_err(|error| format!("Invalid 3MF {name}: {error}"))?;
    if !value.is_finite() {
        return Err(format!("3MF {name} is not finite."));
    }
    Ok(value)
}

fn index(node: Node<'_, '_>, name: &str) -> Result<usize, String> {
    required(node, name)?
        .parse()
        .map_err(|error| format!("Invalid 3MF {name}: {error}"))
}

fn transform(node: Node<'_, '_>) -> Result<DMat4, String> {
    let Some(text) = node.attribute("transform") else {
        return Ok(DMat4::IDENTITY);
    };
    let values = text
        .split_whitespace()
        .map(|value| value.parse::<f64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Invalid 3MF transform: {error}"))?;
    if values.len() != 12 || values.iter().any(|value| !value.is_finite()) {
        return Err("3MF transform must contain 12 finite numbers.".into());
    }
    // 3MF uses row-vector affine transforms; glam uses column vectors.
    Ok(DMat4::from_cols_array(&[
        values[0], values[1], values[2], 0.0, values[3], values[4], values[5], 0.0, values[6],
        values[7], values[8], 0.0, values[9], values[10], values[11], 1.0,
    ]))
}

enum Object {
    Mesh(Vec<RawTriangle>),
    Components(Vec<(usize, DMat4)>),
}

fn parse_model(xml: &str) -> Result<Vec<RawTriangle>, String> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|error| format!("Cannot decode 3MF model: {error}"))?;
    let root = doc.root_element();
    if !root.has_tag_name((CORE_NS, "model")) {
        return Err("3MF model has an unsupported format or namespace.".into());
    }
    if root
        .attribute("requiredextensions")
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Err("3MF requires extensions not supported by this preview.".into());
    }
    let mut objects = HashMap::new();
    let mut vertices_count = 0;
    let mut triangles_count = 0;
    for node in child(root, "resources")?
        .children()
        .filter(|n| n.has_tag_name((CORE_NS, "object")))
    {
        let id = index(node, "id")?;
        let object = if let Some(mesh) = node.children().find(|n| n.has_tag_name((CORE_NS, "mesh")))
        {
            let mut vertices = Vec::new();
            for v in child(mesh, "vertices")?
                .children()
                .filter(|n| n.has_tag_name((CORE_NS, "vertex")))
            {
                vertices_count += 1;
                if vertices_count > VERTEX_LIMIT {
                    return Err("3MF exceeds the 600,000-vertex preview limit.".into());
                }
                vertices.push(DVec3::new(
                    number(v, "x")?,
                    number(v, "y")?,
                    number(v, "z")?,
                ));
            }
            let mut triangles = Vec::new();
            for t in child(mesh, "triangles")?
                .children()
                .filter(|n| n.has_tag_name((CORE_NS, "triangle")))
            {
                triangles_count += 1;
                if triangles_count > TRIANGLE_LIMIT {
                    return Err("3MF exceeds the 200,000-triangle preview limit.".into());
                }
                let vertex = |name| {
                    vertices
                        .get(index(t, name)?)
                        .copied()
                        .ok_or_else(|| "3MF triangle has an invalid vertex index.".to_string())
                };
                push_triangle(
                    &mut triangles,
                    [vertex("v1")?, vertex("v2")?, vertex("v3")?],
                )?;
            }
            Object::Mesh(triangles)
        } else {
            let mut components = Vec::new();
            for c in child(node, "components")?
                .children()
                .filter(|n| n.has_tag_name((CORE_NS, "component")))
            {
                if c.attributes().any(|a| a.name() == "path") {
                    return Err("3MF external component files are not supported.".into());
                }
                components.push((index(c, "objectid")?, transform(c)?));
            }
            Object::Components(components)
        };
        if objects.insert(id, object).is_some() {
            return Err("3MF contains duplicate object IDs.".into());
        }
    }
    let mut triangles = Vec::new();
    let mut visits = 0;
    for item in child(root, "build")?
        .children()
        .filter(|n| n.has_tag_name((CORE_NS, "item")))
    {
        if item.attributes().any(|a| a.name() == "path") {
            return Err("3MF external build items are not supported.".into());
        }
        expand_object(
            index(item, "objectid")?,
            transform(item)?,
            &objects,
            &mut Vec::new(),
            &mut visits,
            &mut triangles,
        )?;
    }
    Ok(triangles)
}

fn expand_object(
    id: usize,
    transform: DMat4,
    objects: &HashMap<usize, Object>,
    stack: &mut Vec<usize>,
    visits: &mut usize,
    triangles: &mut Vec<RawTriangle>,
) -> Result<(), String> {
    *visits += 1;
    if *visits > TRIANGLE_LIMIT || stack.len() >= 32 {
        return Err("3MF component hierarchy exceeds preview limits.".into());
    }
    if stack.contains(&id) {
        return Err("3MF component hierarchy contains a cycle.".into());
    }
    match objects
        .get(&id)
        .ok_or_else(|| format!("3MF references missing object {id}."))?
    {
        Object::Mesh(mesh) => {
            for triangle in mesh {
                push_triangle(
                    triangles,
                    triangle.map(|vertex| transform.transform_point3(vertex)),
                )?;
            }
        }
        Object::Components(components) => {
            stack.push(id);
            for (child, local) in components {
                expand_object(*child, transform * local, objects, stack, visits, triangles)?;
            }
            stack.pop();
        }
    }
    Ok(())
}

fn normalize(triangles: Vec<RawTriangle>) -> Result<Vec<Triangle>, String> {
    if triangles.is_empty() {
        return Err("3D model contains no triangles.".into());
    }
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    for vertex in triangles.iter().flatten() {
        min = min.min(*vertex);
        max = max.max(*vertex);
    }
    let center = min * 0.5 + max * 0.5;
    let radius = triangles
        .iter()
        .flatten()
        .map(|v| (*v - center).length())
        .fold(0.0_f64, f64::max);
    if !radius.is_finite() || radius <= 0.0 {
        return Err("3D model has invalid or zero-sized bounds.".into());
    }
    let triangles: Vec<_> = triangles
        .into_iter()
        .map(|t| t.map(|v| ((v - center) / radius).as_vec3()))
        .collect();
    if !triangles
        .iter()
        .any(|t| (t[1] - t[0]).cross(t[2] - t[0]).length_squared() > 1e-16)
    {
        return Err("3D model contains no renderable triangles.".into());
    }
    Ok(triangles)
}

fn render(triangles: &[Triangle], view: View) -> egui::ColorImage {
    let [width, height] = view.size;
    let mut image = egui::ColorImage::filled(view.size, BACKGROUND);
    let mut depth = vec![f32::NEG_INFINITY; width * height];
    let rotation = Mat3::from_rotation_x(view.pitch) * Mat3::from_rotation_z(view.yaw);
    let scale = width.min(height) as f32 * 0.44 * view.zoom;
    let light = Vec3::new(-0.4, 0.6, 1.0).normalize();
    for triangle in triangles {
        let rotated = triangle.map(|v| rotation * v);
        let normal = (rotated[1] - rotated[0])
            .cross(rotated[2] - rotated[0])
            .normalize_or_zero();
        let intensity = 0.25 + 0.75 * normal.dot(light).abs();
        let color = egui::Color32::from_rgb(
            (80.0 * intensity) as u8,
            (150.0 * intensity) as u8,
            (210.0 * intensity) as u8,
        );
        let points = rotated.map(|v| {
            Vec3::new(
                width as f32 * 0.5 + v.x * scale,
                height as f32 * 0.5 - v.y * scale,
                v.z,
            )
        });
        raster_triangle(&mut image, &mut depth, points, color);
    }
    image
}

fn edge(a: Vec3, b: Vec3, p: Vec3) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}

fn raster_triangle(
    image: &mut egui::ColorImage,
    depth: &mut [f32],
    points: Triangle,
    color: egui::Color32,
) {
    let [a, b, c] = points;
    let area = edge(a, b, c);
    if area.abs() < 1e-8 {
        return;
    }
    let [width, height] = image.size;
    let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as usize;
    let max_x = a.x.max(b.x).max(c.x).ceil().min(width as f32 - 1.0) as isize;
    let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as usize;
    let max_y = a.y.max(b.y).max(c.y).ceil().min(height as f32 - 1.0) as isize;
    if max_x < min_x as isize || max_y < min_y as isize {
        return;
    }
    for y in min_y..=max_y as usize {
        for x in min_x..=max_x as usize {
            let p = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, 0.0);
            let weights = [
                edge(b, c, p) / area,
                edge(c, a, p) / area,
                edge(a, b, p) / area,
            ];
            if weights.iter().all(|w| *w >= -1e-6) {
                let z = weights[0] * a.z + weights[1] * b.z + weights[2] * c.z;
                let index = y * width + x;
                if z > depth[index] {
                    depth[index] = z;
                    image.pixels[index] = color;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const ASCII_STL: &str = "solid test
            facet normal 0 0 1
            outer loop
            vertex 0 0 0
            vertex 2 0 0
            vertex 0 1 0
            endloop
            endfacet
            endsolid test";

    fn mesh() -> String {
        "<object id=\"1\" type=\"model\"><mesh><vertices>
                    <vertex x=\"0\" y=\"0\" z=\"0\"/><vertex x=\"2\" y=\"0\" z=\"0\"/>
                    <vertex x=\"0\" y=\"1\" z=\"0\"/></vertices>
                    <triangles><triangle v1=\"0\" v2=\"1\" v3=\"2\"/></triangles></mesh></object>"
            .into()
    }

    fn model(resources: &str, build: &str) -> String {
        format!(
            "<model xmlns=\"{CORE_NS}\"><resources>{resources}</resources><build>{build}</build></model>"
        )
    }

    fn package(xml: &str) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("_rels/.rels", options).unwrap();
        write!(zip, "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">
                        <Relationship Id=\"model\" Type=\"{MODEL_REL}\" Target=\"/custom/part.model\"/>
                        </Relationships>").unwrap();
        zip.start_file("custom/part.model", options).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn loads_ascii_and_binary_stl_and_dispatches_both_model_extensions() {
        let ascii = load_stl(ASCII_STL.as_bytes()).unwrap();
        let mut binary = vec![0; 80];
        binary.extend_from_slice(&1_u32.to_le_bytes());
        for value in [
            0.0_f32, 0.0, 1.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 1.0, 0.0,
        ] {
            binary.extend_from_slice(&value.to_le_bytes());
        }
        binary.extend_from_slice(&0_u16.to_le_bytes());
        assert_eq!(load_stl(&binary).unwrap(), ascii);
        assert!(load_stl(&binary[..binary.len() - 1]).is_err());
        assert!(load_stl(b"not a model").is_err());
        let dir = tempfile::tempdir().unwrap();
        let xml = model(&mesh(), "<item objectid=\"1\"/>");
        for (name, bytes) in [("part.STL", binary), ("part.3MF", package(&xml))] {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            assert!(matches!(
                crate::preview::load(&path).unwrap().content,
                crate::preview::Content::Model(_)
            ));
        }
    }

    #[test]
    fn resolves_package_relationships_and_composes_component_and_build_transforms() {
        let resources = format!(
            "{}<object id=\"2\"><components>
                        <component objectid=\"1\" transform=\"2 0 0 0 2 0 0 0 2 3 0 0\"/>
                        </components></object>",
            mesh()
        );
        let xml = model(
            &resources,
            "<item objectid=\"2\" transform=\"0 1 0 -1 0 0 0 0 1 10 20 0\"/><item objectid=\"1\"/>",
        );
        let triangles = load_3mf(Cursor::new(package(&xml))).unwrap();
        assert_eq!(triangles.len(), 2);
        assert_eq!(
            triangles[0],
            [
                DVec3::new(10.0, 23.0, 0.0),
                DVec3::new(10.0, 27.0, 0.0),
                DVec3::new(8.0, 23.0, 0.0),
            ]
        );
        assert_eq!(triangles[1][0], DVec3::ZERO);
    }

    #[test]
    fn rejects_invalid_3mf_geometry_and_unsupported_features() {
        let build = "<item objectid=\"1\"/>";
        for xml in [
            model(&mesh().replace("v3=\"2\"", "v3=\"9\""), build),
            model(&mesh().replace("x=\"2\"", "x=\"NaN\""), build),
            model(&mesh(), "<item objectid=\"9\"/>"),
            model(&format!("{}{}", mesh(), mesh()), build),
            model(
                "<object id=\"1\"><components><component objectid=\"1\"/></components></object>",
                build,
            ),
            model(&mesh(), "<item objectid=\"1\" transform=\"1 2 3\"/>"),
            model(&mesh(), build).replace("<model ", "<model requiredextensions=\"p\" "),
            model(
                &mesh(),
                "<item xmlns:p=\"urn:production\" p:path=\"/other.model\" objectid=\"1\"/>",
            ),
        ] {
            assert!(parse_model(&xml).is_err(), "{xml}");
        }
        assert!(load_3mf(Cursor::new(b"not a zip")).is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.3mf");
        std::fs::write(&path, b"not a zip").unwrap();
        assert!(crate::preview::load(&path).is_err());
    }

    #[test]
    fn enforces_geometry_and_file_limits_and_rejects_empty_models() {
        let mut triangles = vec![[DVec3::ZERO; 3]; TRIANGLE_LIMIT];
        assert!(push_triangle(&mut triangles, [DVec3::ZERO; 3]).is_err());
        assert!(push_triangle(&mut Vec::new(), [DVec3::splat(f64::NAN); 3]).is_err());
        assert!(normalize(Vec::new()).is_err());
        assert!(normalize(vec![[DVec3::ZERO; 3]]).is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.stl");
        File::create(&path)
            .unwrap()
            .set_len(FILE_LIMIT + 1)
            .unwrap();
        assert!(ModelPreview::load(&path).err().unwrap().contains("64 MiB"));
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("large.model", zip::write::SimpleFileOptions::default())
            .unwrap();
        let chunk = [b' '; 64 * 1024];
        for _ in 0..(XML_LIMIT / chunk.len() as u64) {
            zip.write_all(&chunk).unwrap();
        }
        zip.write_all(b" ").unwrap();
        let mut archive =
            zip::ZipArchive::new(Cursor::new(zip.finish().unwrap().into_inner())).unwrap();
        assert!(
            read_entry(&mut archive, "large.model")
                .unwrap_err()
                .contains("32 MiB")
        );
    }

    #[test]
    fn depth_test_is_order_independent_and_clips_zoomed_triangles() {
        let front = [
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(8.0, 0.0, 1.0),
            Vec3::new(0.0, 8.0, 1.0),
        ];
        let back = front.map(|v| v - Vec3::Z);
        for order in [true, false] {
            let mut image = egui::ColorImage::filled([8, 8], BACKGROUND);
            let mut depth = vec![f32::NEG_INFINITY; 64];
            let faces = if order {
                [(front, egui::Color32::RED), (back, egui::Color32::BLUE)]
            } else {
                [(back, egui::Color32::BLUE), (front, egui::Color32::RED)]
            };
            for (triangle, color) in faces {
                raster_triangle(&mut image, &mut depth, triangle, color);
            }
            assert_eq!(image.pixels[9], egui::Color32::RED);
            assert_eq!(image.pixels[63], BACKGROUND);
            raster_triangle(
                &mut image,
                &mut depth,
                front.map(|v| v - Vec3::splat(100.0)),
                egui::Color32::GREEN,
            );
            assert_eq!(image.pixels[9], egui::Color32::RED);
        }
    }

    #[test]
    fn normalized_mesh_renders_and_zoom_and_rotation_change_the_image() {
        let triangles = normalize(load_stl(ASCII_STL.as_bytes()).unwrap()).unwrap();
        let mut view = View {
            size: [80, 60],
            yaw: 0.0,
            pitch: 0.0,
            zoom: 1.0,
        };
        let image = render(&triangles, view);
        let count =
            |image: &egui::ColorImage| image.pixels.iter().filter(|p| **p != BACKGROUND).count();
        assert!(count(&image) > 100);
        view.scroll(300.0);
        let zoomed = render(&triangles, view);
        assert!(count(&zoomed) > count(&image));
        view.drag(egui::vec2(35.0, 70.0));
        assert_ne!(render(&triangles, view).pixels, zoomed.pixels);
        view.scroll(f32::MAX);
        assert_eq!(view.zoom, 10.0);
        view.scroll(-f32::MAX);
        assert_eq!(view.zoom, 0.1);
    }

    #[test]
    fn pane_handles_scroll_and_only_left_button_rotation() {
        let ctx = egui::Context::default();
        let (requests, _pending) = mpsc::sync_channel(1);
        let (_sender, results) = mpsc::sync_channel(1);
        let mut preview = ModelPreview {
            requests,
            results,
            view: View {
                size: [1, 1],
                yaw: 0.0,
                pitch: 0.0,
                zoom: 1.0,
            },
            submitted: None,
            displayed: None,
            texture: None,
            error: None,
        };
        let mut frame = |events| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(200.0, 200.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| preview.show(ui));
                },
            );
            preview.view
        };
        let wheel = |delta| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, delta),
            modifiers: egui::Modifiers::NONE,
        };
        let moved = |x, y| egui::Event::PointerMoved(egui::pos2(x, y));
        let button = |button, pressed, x, y| egui::Event::PointerButton {
            pos: egui::pos2(x, y),
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![moved(20.0, 20.0)]);
        assert!(frame(vec![wheel(100.0)]).zoom > 1.0);
        assert!((frame(vec![wheel(-100.0)]).zoom - 1.0).abs() < 1e-5);
        frame(vec![button(
            egui::PointerButton::Secondary,
            true,
            20.0,
            20.0,
        )]);
        assert_eq!(frame(vec![moved(60.0, 60.0)]).yaw, 0.0);
        frame(vec![button(
            egui::PointerButton::Secondary,
            false,
            60.0,
            60.0,
        )]);
        frame(vec![button(egui::PointerButton::Primary, true, 60.0, 60.0)]);
        let rotated = frame(vec![moved(100.0, 110.0)]);
        assert!(rotated.yaw > 0.0 && rotated.pitch > 0.0);
        frame(vec![button(
            egui::PointerButton::Primary,
            false,
            100.0,
            110.0,
        )]);
    }
}
