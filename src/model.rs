use eframe::egui;
use glam::{DMat4, DVec3, Mat3, Vec3};
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::{QName, ResolveResult};
use quick_xml::reader::NsReader;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Read, Seek};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::time::Duration;

const FILE_LIMIT: u64 = 64 * 1024 * 1024;
const XML_LIMIT: u64 = 256 * 1024 * 1024;
const PACKAGE_XML_LIMIT: u64 = 256 * 1024 * 1024;
const PART_LIMIT: usize = 128;
const COMPONENT_VISIT_LIMIT: usize = 200_000;
const STL_TRIANGLE_LIMIT: usize = 200_000;
const TRIANGLE_LIMIT: usize = 2_000_000;
const VERTEX_LIMIT: usize = 1_000_000;
const CORE_NS: &str = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02";
const PRODUCTION_NS: &str = "http://schemas.microsoft.com/3dmanufacturing/production/2015/06";
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
    pan: egui::Vec2,
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
                pan: egui::Vec2::ZERO,
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
        if response.dragged_by(egui::PointerButton::Middle) {
            let delta = ui.input(|input| input.pointer.delta());
            self.view.pan += egui::vec2(
                delta.x / rect.width().max(1.0),
                delta.y / rect.height().max(1.0),
            );
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
        response.on_hover_text("Scroll to zoom. Left-drag to rotate. Middle-drag to pan.");
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
            STL_TRIANGLE_LIMIT,
        )?;
    }
    Ok(triangles)
}

fn push_triangle(
    triangles: &mut Vec<RawTriangle>,
    triangle: RawTriangle,
    limit: usize,
) -> Result<(), String> {
    if triangles.len() >= limit {
        return Err(format!(
            "3D model exceeds the {limit}-triangle preview limit."
        ));
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
    limit: u64,
) -> Result<String, String> {
    let entry = archive
        .by_name(name)
        .map_err(|error| format!("Cannot read 3MF entry {name}: {error}"))?;
    if entry.size() > limit {
        return Err(format!(
            "3MF entry {name} exceeds the XML preview budget ({limit} bytes)."
        ));
    }
    let mut bytes = Vec::new();
    entry
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot decompress 3MF entry {name}: {error}"))?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "3MF entry {name} exceeds the XML preview budget ({limit} bytes)."
        ));
    }
    String::from_utf8(bytes).map_err(|error| format!("3MF XML is not UTF-8: {error}"))
}

fn load_3mf<R: Read + Seek>(reader: R) -> Result<Vec<RawTriangle>, String> {
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|error| format!("Cannot open 3MF: {error}"))?;
    let relationships = read_entry(&mut archive, "_rels/.rels", XML_LIMIT)?;
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
    let root_path = resolve_part_path(
        "",
        model
            .attribute("Target")
            .ok_or("3MF Relationship is missing Target.")?,
    )?;
    let mut models = HashMap::new();
    let mut queued = HashSet::from([root_path.clone()]);
    let mut pending = vec![root_path.clone()];
    let mut budget = ModelBudget {
        xml_bytes: relationships.len() as u64,
        ..Default::default()
    };
    while let Some(path) = pending.pop() {
        let limit = XML_LIMIT.min(PACKAGE_XML_LIMIT - budget.xml_bytes);
        let entry = archive
            .by_name(&path)
            .map_err(|error| format!("Cannot read 3MF entry {path}: {error}"))?;
        if entry.size() > limit {
            return Err(format!(
                "3MF entry {path} exceeds the XML preview budget ({limit} bytes)."
            ));
        }
        let part = parse_model(
            BufReader::new(entry.take(limit + 1)),
            &path,
            &mut budget,
            limit,
        )?;
        let references = part
            .objects
            .values()
            .flat_map(|object| match object {
                Object::Mesh(_) => [].iter(),
                Object::Components(components) => components.iter(),
            })
            .chain(part.build.iter());
        for reference in references {
            if queued.insert(reference.part.clone()) {
                if queued.len() > PART_LIMIT {
                    return Err("3MF exceeds the 128-model-part preview limit.".into());
                }
                pending.push(reference.part.clone());
            }
        }
        models.insert(path, part);
    }
    let mut triangles = Vec::new();
    let mut visits = 0;
    for item in &models[&root_path].build {
        expand_object(
            item,
            DMat4::IDENTITY,
            &models,
            &mut Vec::new(),
            &mut visits,
            &mut triangles,
        )?;
    }
    Ok(triangles)
}

fn resolve_part_path(base: &str, target: &str) -> Result<String, String> {
    let mut parts = Vec::new();
    if !target.starts_with('/')
        && let Some((parent, _)) = base.rsplit_once('/')
    {
        parts.extend(parent.split('/'));
    }
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or("Invalid 3MF model path.")?;
            }
            part if part.contains([':', '\\', '\0', '?', '#']) => {
                return Err("Invalid 3MF model path.".into());
            }
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err("Invalid 3MF model path.".into());
    }
    Ok(parts.join("/"))
}

struct ModelElement {
    name: String,
    attributes: Vec<(Option<String>, String, String)>,
}

impl ModelElement {
    fn read<R: BufRead>(event: &BytesStart<'_>, reader: &NsReader<R>) -> Result<Self, String> {
        let mut attributes = Vec::new();
        for attribute in event.attributes() {
            let attribute = attribute.map_err(|error| format!("Invalid 3MF attribute: {error}"))?;
            if attribute.key.as_ref() == b"xmlns" || attribute.key.as_ref().starts_with(b"xmlns:") {
                continue;
            }
            let (namespace, name) = reader.resolver().resolve_attribute(attribute.key);
            let namespace = match namespace {
                ResolveResult::Unbound => None,
                ResolveResult::Bound(namespace) => Some(
                    std::str::from_utf8(namespace.as_ref())
                        .map_err(|error| format!("Invalid 3MF namespace: {error}"))?
                        .to_string(),
                ),
                ResolveResult::Unknown(_) => return Err("3MF uses an unbound namespace.".into()),
            };
            let name = std::str::from_utf8(name.as_ref())
                .map_err(|error| format!("Invalid 3MF attribute name: {error}"))?
                .to_string();
            if attributes
                .iter()
                .any(|(ns, key, _)| ns == &namespace && key == &name)
            {
                return Err("3MF contains duplicate attributes.".into());
            }
            let value = attribute
                .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                .map_err(|error| format!("Invalid 3MF attribute: {error}"))?
                .into_owned();
            attributes.push((namespace, name, value));
        }
        Ok(Self {
            name: std::str::from_utf8(event.local_name().as_ref())
                .map_err(|error| format!("Invalid 3MF element name: {error}"))?
                .to_string(),
            attributes,
        })
    }

    fn attribute(&self, namespace: Option<&str>, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(ns, key, _)| ns.as_deref() == namespace && key == name)
            .map(|(_, _, value)| value.as_str())
    }
}

fn required<'a>(node: &'a ModelElement, name: &str) -> Result<&'a str, String> {
    node.attribute(None, name)
        .ok_or_else(|| format!("3MF {} is missing {name}.", node.name))
}

fn number(node: &ModelElement, name: &str) -> Result<f64, String> {
    let value: f64 = required(node, name)?
        .parse()
        .map_err(|error| format!("Invalid 3MF {name}: {error}"))?;
    if !value.is_finite() {
        return Err(format!("3MF {name} is not finite."));
    }
    Ok(value)
}

fn index(node: &ModelElement, name: &str) -> Result<usize, String> {
    required(node, name)?
        .parse()
        .map_err(|error| format!("Invalid 3MF {name}: {error}"))
}

fn transform(node: &ModelElement) -> Result<DMat4, String> {
    let Some(text) = node.attribute(None, "transform") else {
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
    Components(Vec<ObjectReference>),
}

struct ObjectReference {
    part: String,
    id: usize,
    transform: DMat4,
}

struct ModelPart {
    objects: HashMap<usize, Object>,
    build: Vec<ObjectReference>,
}

#[derive(Default)]
struct ModelBudget {
    vertices: usize,
    triangles: usize,
    xml_bytes: u64,
}

fn object_reference(node: &ModelElement, part: &str, unit: f64) -> Result<ObjectReference, String> {
    if node
        .attributes
        .iter()
        .any(|(ns, name, _)| name == "path" && ns.as_deref() != Some(PRODUCTION_NS))
    {
        return Err("3MF model reference uses an unsupported path namespace.".into());
    }
    let path = match node.attribute(Some(PRODUCTION_NS), "path") {
        Some(path) => resolve_part_path(part, path)?,
        None => part.to_string(),
    };
    let mut matrix = transform(node)?;
    matrix.w_axis.x *= unit;
    matrix.w_axis.y *= unit;
    matrix.w_axis.z *= unit;
    Ok(ObjectReference {
        part: path,
        id: index(node, "objectid")?,
        transform: matrix,
    })
}

#[derive(Clone, Copy, PartialEq)]
enum ModelTag {
    Model,
    Resources,
    ObjectNode,
    Mesh,
    Vertices,
    Vertex,
    Triangles,
    Triangle,
    Components,
    Component,
    Build,
    Item,
    Ignored,
}

#[derive(Default)]
struct PendingObject {
    id: usize,
    mesh: bool,
    vertices: Option<Vec<DVec3>>,
    triangles: Option<Vec<RawTriangle>>,
    components: Option<Vec<ObjectReference>>,
}

fn parse_model<R: BufRead>(
    input: R,
    part: &str,
    budget: &mut ModelBudget,
    limit: u64,
) -> Result<ModelPart, String> {
    use ModelTag::*;
    let mut reader = NsReader::from_reader(input);
    reader.config_mut().expand_empty_elements = true;
    reader.config_mut().check_comments = true;
    let mut buffer = Vec::new();
    let mut stack = Vec::new();
    let mut root_seen = false;
    let mut resources_seen = false;
    let mut build_seen = false;
    let mut declaration_seen = false;
    let mut unit = 1.0;
    let mut object = PendingObject::default();
    let mut objects = HashMap::new();
    let mut build = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| format!("Cannot decode 3MF model {part}: {error}"))?;
        if reader.buffer_position() > limit {
            return Err(format!(
                "3MF entry {part} exceeds the XML preview budget ({limit} bytes)."
            ));
        }
        match event {
            Event::Start(event) => {
                if stack.len() >= 128 {
                    return Err("3MF XML exceeds the 128-level nesting limit.".into());
                }
                let (namespace, _) = reader.resolver().resolve_element(event.name());
                if matches!(namespace, ResolveResult::Unknown(_)) {
                    return Err("3MF uses an unbound namespace.".into());
                }
                let core = matches!(namespace, ResolveResult::Bound(ns) if ns.as_ref() == CORE_NS.as_bytes());
                let node = ModelElement::read(&event, &reader)?;
                let tag = if stack.is_empty() {
                    if root_seen || !core || node.name != "model" {
                        return Err("3MF model has an unsupported format or namespace.".into());
                    }
                    root_seen = true;
                    for prefix in node
                        .attribute(None, "requiredextensions")
                        .unwrap_or("")
                        .split_whitespace()
                    {
                        let name = format!("{prefix}:extension");
                        if !matches!(
                            reader.resolver().resolve_element(QName(name.as_bytes())).0,
                            ResolveResult::Bound(ns) if ns.as_ref() == PRODUCTION_NS.as_bytes() && !prefix.contains(':')
                        ) {
                            return Err(format!(
                                "3MF requires an unsupported extension: {prefix}."
                            ));
                        }
                    }
                    unit = match node.attribute(None, "unit").unwrap_or("millimeter") {
                        "micron" => 0.001,
                        "millimeter" => 1.0,
                        "centimeter" => 10.0,
                        "inch" => 25.4,
                        "foot" => 304.8,
                        "meter" => 1000.0,
                        unit => return Err(format!("Unsupported 3MF model unit: {unit}.")),
                    };
                    Model
                } else if core {
                    match (stack.last(), node.name.as_str()) {
                        (Some(Model), "resources") => {
                            if resources_seen {
                                return Err("3MF model contains duplicate resources.".into());
                            }
                            resources_seen = true;
                            Resources
                        }
                        (Some(Resources), "object") => {
                            object = PendingObject {
                                id: index(&node, "id")?,
                                ..Default::default()
                            };
                            ObjectNode
                        }
                        (Some(ObjectNode), "mesh") => {
                            if object.mesh {
                                return Err("3MF object contains duplicate meshes.".into());
                            }
                            object.mesh = true;
                            Mesh
                        }
                        (Some(Mesh), "vertices") => {
                            if object.vertices.is_some() {
                                return Err("3MF mesh contains duplicate vertices sections.".into());
                            }
                            object.vertices = Some(Vec::new());
                            Vertices
                        }
                        (Some(Vertices), "vertex") => {
                            budget.vertices += 1;
                            if budget.vertices > VERTEX_LIMIT {
                                return Err(
                                    "3MF exceeds the 1,000,000-vertex preview limit.".into()
                                );
                            }
                            object.vertices.as_mut().unwrap().push(
                                DVec3::new(
                                    number(&node, "x")?,
                                    number(&node, "y")?,
                                    number(&node, "z")?,
                                ) * unit,
                            );
                            Vertex
                        }
                        (Some(Mesh), "triangles") => {
                            if object.triangles.is_some() {
                                return Err(
                                    "3MF mesh contains duplicate triangles sections.".into()
                                );
                            }
                            object.triangles = Some(Vec::new());
                            Triangles
                        }
                        (Some(Triangles), "triangle") => {
                            budget.triangles += 1;
                            if budget.triangles > TRIANGLE_LIMIT {
                                return Err(
                                    "3MF exceeds the 2,000,000-triangle preview limit.".into()
                                );
                            }
                            let vertices = object
                                .vertices
                                .as_ref()
                                .ok_or("3MF mesh is missing vertices before triangles.")?;
                            let vertex = |name| {
                                vertices.get(index(&node, name)?).copied().ok_or_else(|| {
                                    "3MF triangle has an invalid vertex index.".to_string()
                                })
                            };
                            let triangle = [vertex("v1")?, vertex("v2")?, vertex("v3")?];
                            push_triangle(
                                object.triangles.as_mut().unwrap(),
                                triangle,
                                TRIANGLE_LIMIT,
                            )?;
                            Triangle
                        }
                        (Some(ObjectNode), "components") => {
                            if object.components.is_some() {
                                return Err(
                                    "3MF object contains duplicate components sections.".into()
                                );
                            }
                            object.components = Some(Vec::new());
                            Components
                        }
                        (Some(Components), "component") => {
                            object
                                .components
                                .as_mut()
                                .unwrap()
                                .push(object_reference(&node, part, unit)?);
                            Component
                        }
                        (Some(Model), "build") => {
                            if build_seen {
                                return Err("3MF model contains duplicate build sections.".into());
                            }
                            build_seen = true;
                            Build
                        }
                        (Some(Build), "item") => {
                            build.push(object_reference(&node, part, unit)?);
                            Item
                        }
                        _ => Ignored,
                    }
                } else {
                    Ignored
                };
                stack.push(tag);
            }
            Event::End(_) => {
                if stack.pop() == Some(ObjectNode) {
                    let pending = std::mem::take(&mut object);
                    let value = if pending.mesh {
                        pending.vertices.ok_or("3MF mesh is missing vertices.")?;
                        Object::Mesh(pending.triangles.ok_or("3MF mesh is missing triangles.")?)
                    } else {
                        Object::Components(
                            pending
                                .components
                                .ok_or("3MF object is missing components.")?,
                        )
                    };
                    if objects.insert(pending.id, value).is_some() {
                        return Err("3MF contains duplicate object IDs.".into());
                    }
                }
            }
            Event::DocType(_) => return Err("3MF XML must not contain a DTD.".into()),
            Event::Decl(declaration) => {
                if root_seen || declaration_seen {
                    return Err("3MF XML declaration must precede the model.".into());
                }
                declaration
                    .xml_version()
                    .map_err(|error| format!("Invalid 3MF XML declaration: {error}"))?;
                declaration_seen = true;
            }
            Event::Text(text) => {
                let text = text
                    .decode()
                    .map_err(|error| format!("Invalid 3MF XML text: {error}"))?;
                if stack.is_empty() && !text.trim().is_empty() {
                    return Err("3MF XML contains text outside the model.".into());
                }
            }
            Event::GeneralRef(reference) => {
                let name = reference
                    .decode()
                    .map_err(|error| format!("Invalid 3MF XML reference: {error}"))?;
                if stack.is_empty()
                    || (reference
                        .resolve_char_ref()
                        .map_err(|error| format!("Invalid 3MF XML reference: {error}"))?
                        .is_none()
                        && quick_xml::escape::resolve_predefined_entity(&name).is_none())
                {
                    return Err("3MF XML contains an unsupported entity reference.".into());
                }
            }
            Event::CData(text) => {
                text.decode()
                    .map_err(|error| format!("Invalid 3MF XML text: {error}"))?;
                if stack.is_empty() {
                    return Err("3MF XML contains CDATA outside the model.".into());
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !root_seen || !stack.is_empty() || !resources_seen {
        return Err("3MF model is incomplete or missing resources.".into());
    }
    budget.xml_bytes += reader.buffer_position();
    Ok(ModelPart { objects, build })
}

fn expand_object(
    reference: &ObjectReference,
    parent: DMat4,
    models: &HashMap<String, ModelPart>,
    stack: &mut Vec<(String, usize)>,
    visits: &mut usize,
    triangles: &mut Vec<RawTriangle>,
) -> Result<(), String> {
    *visits += 1;
    if *visits > COMPONENT_VISIT_LIMIT || stack.len() >= 32 {
        return Err("3MF component hierarchy exceeds preview limits.".into());
    }
    let key = (reference.part.clone(), reference.id);
    if stack.contains(&key) {
        return Err("3MF component hierarchy contains a cycle.".into());
    }
    let transform = parent * reference.transform;
    let model = models
        .get(&reference.part)
        .ok_or_else(|| format!("3MF references missing model part {}.", reference.part))?;
    match model.objects.get(&reference.id).ok_or_else(|| {
        format!(
            "3MF references missing object {} in {}.",
            reference.id, reference.part
        )
    })? {
        Object::Mesh(mesh) => {
            for triangle in mesh {
                push_triangle(
                    triangles,
                    triangle.map(|vertex| transform.transform_point3(vertex)),
                    TRIANGLE_LIMIT,
                )?;
            }
        }
        Object::Components(components) => {
            stack.push(key);
            for child in components {
                expand_object(child, transform, models, stack, visits, triangles)?;
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
                width as f32 * (0.5 + view.pan.x) + v.x * scale,
                height as f32 * (0.5 + view.pan.y) - v.y * scale,
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

    #[test]
    #[ignore = "Set HOP_3MF_SAMPLE to a local 3MF file to validate its background rendering"]
    fn renders_supplied_3mf_sample_with_zoom_and_rotation() {
        let path = std::env::var_os("HOP_3MF_SAMPLE").expect("Set HOP_3MF_SAMPLE");
        let start = std::time::Instant::now();
        let preview = ModelPreview::load(Path::new(&path)).unwrap();
        let loaded = start.elapsed();
        let mut view = View {
            size: [800, 560],
            ..preview.view
        };
        preview.requests.send(view).unwrap();
        let (_, initial) = preview
            .results
            .recv_timeout(Duration::from_secs(60))
            .unwrap();
        let visible = initial
            .pixels
            .iter()
            .filter(|pixel| **pixel != BACKGROUND)
            .count();
        assert!(visible > 100, "Expected visible model surfaces");
        view.scroll(200.0);
        view.drag(egui::vec2(35.0, 70.0));
        preview.requests.send(view).unwrap();
        let (_, changed) = preview
            .results
            .recv_timeout(Duration::from_secs(60))
            .unwrap();
        assert!(changed.pixels.iter().any(|pixel| *pixel != BACKGROUND));
        assert_ne!(initial.pixels, changed.pixels);
        println!(
            "Loaded in {loaded:?}; rendered {visible} surface pixels; zoom/rotation verified."
        );
    }

    #[test]
    #[ignore = "Set HOP_3MF_SAMPLE to validate streaming XML parsing without expanding instances"]
    fn parses_supplied_3mf_sample_within_source_geometry_limits() {
        let path = std::env::var_os("HOP_3MF_SAMPLE").expect("Set HOP_3MF_SAMPLE");
        let mut archive = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut budget = ModelBudget::default();
        let start = std::time::Instant::now();
        let mut parts = 0;
        for i in 0..archive.len() {
            let entry = archive.by_index(i).unwrap();
            if !entry.name().ends_with(".model") {
                continue;
            }
            let name = entry.name().to_string();
            let limit = XML_LIMIT.min(PACKAGE_XML_LIMIT - budget.xml_bytes);
            assert!(entry.size() <= limit);
            parse_model(
                BufReader::new(entry.take(limit + 1)),
                &name,
                &mut budget,
                limit,
            )
            .unwrap();
            parts += 1;
        }
        assert!(parts > 0 && budget.triangles > 0);
        println!(
            "Parsed {parts} parts, {} XML bytes, {} vertices and {} source triangles in {:?}; instances not expanded.",
            budget.xml_bytes,
            budget.vertices,
            budget.triangles,
            start.elapsed()
        );
    }

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
        package_parts(&[("custom/part.model", xml)])
    }

    fn package_parts(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("_rels/.rels", options).unwrap();
        write!(
            zip,
            "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">
                        <Relationship Id=\"model\" Type=\"{MODEL_REL}\" Target=\"/{}\"/>
                        </Relationships>",
            parts[0].0
        )
        .unwrap();
        for &(path, xml) in parts {
            zip.start_file(path, options).unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
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
            assert!(load_3mf(Cursor::new(package(&xml))).is_err(), "{xml}");
        }
        assert!(load_3mf(Cursor::new(b"not a zip")).is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.3mf");
        std::fs::write(&path, b"not a zip").unwrap();
        assert!(crate::preview::load(&path).is_err());
    }

    #[test]
    fn enforces_geometry_and_file_limits_and_rejects_empty_models() {
        let mut triangles = vec![[DVec3::ZERO; 3]; 2];
        assert!(push_triangle(&mut triangles, [DVec3::ZERO; 3], 2).is_err());
        assert!(push_triangle(&mut Vec::new(), [DVec3::splat(f64::NAN); 3], 2).is_err());
        for mut budget in [
            ModelBudget {
                vertices: VERTEX_LIMIT - 2,
                ..Default::default()
            },
            ModelBudget {
                triangles: TRIANGLE_LIMIT,
                ..Default::default()
            },
        ] {
            assert!(
                parse_model(
                    model(&mesh(), "").as_bytes(),
                    "test.model",
                    &mut budget,
                    XML_LIMIT
                )
                .is_err()
            );
        }
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
        zip.start_file(
            "large.model",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(b"more than eight bytes").unwrap();
        let mut archive =
            zip::ZipArchive::new(Cursor::new(zip.finish().unwrap().into_inner())).unwrap();
        assert!(
            read_entry(&mut archive, "large.model", 8)
                .unwrap_err()
                .contains("XML preview budget")
        );
    }

    #[test]
    fn streams_model_parts_larger_than_the_previous_128_mib_limit() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("_rels/.rels", options).unwrap();
        write!(
            zip,
            "<Relationships><Relationship Type=\"{MODEL_REL}\" Target=\"large.model\"/></Relationships>"
        ).unwrap();
        zip.start_file("large.model", options).unwrap();
        write!(zip, "<model xmlns=\"{CORE_NS}\">").unwrap();
        let padding = format!("<!--{}-->", " ".repeat(64 * 1024));
        for _ in 0..2048 {
            zip.write_all(padding.as_bytes()).unwrap();
        }
        write!(
            zip,
            "<resources>{}</resources><build><item objectid=\"1\"/></build></model>",
            mesh()
        )
        .unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
        assert!(archive.by_name("large.model").unwrap().size() > 128 * 1024 * 1024);
        let triangles = load_3mf(Cursor::new(bytes)).unwrap();
        assert_eq!(
            triangles,
            vec![[DVec3::ZERO, DVec3::new(2.0, 0.0, 0.0), DVec3::Y]]
        );
    }

    #[test]
    fn streaming_parser_enforces_byte_budget_and_rejects_incomplete_xml() {
        let xml = model(&mesh(), "<item objectid=\"1\"/>");
        let mut budget = ModelBudget::default();
        parse_model(
            BufReader::with_capacity(3, xml.as_bytes()),
            "test.model",
            &mut budget,
            xml.len() as u64,
        )
        .unwrap();
        assert_eq!(budget.xml_bytes, xml.len() as u64);
        assert!(
            parse_model(
                xml.as_bytes(),
                "test.model",
                &mut ModelBudget::default(),
                xml.len() as u64 - 1
            )
            .err()
            .unwrap()
            .contains("XML preview budget")
        );
        for invalid in [
            xml.trim_end_matches("</model>").to_string(),
            format!("{xml}{xml}"),
            format!("<!DOCTYPE model>{xml}"),
            xml.replace("</model>", "&unknown;</model>"),
            xml.replace("</model>", "</resources>"),
            xml.replace("<resources>", ""),
            xml.replace(
                "</model>",
                &format!("{}{}", "<nested>".repeat(128), "</nested>".repeat(128)),
            ),
        ] {
            assert!(
                parse_model(
                    invalid.as_bytes(),
                    "test.model",
                    &mut ModelBudget::default(),
                    XML_LIMIT
                )
                .is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn streaming_parser_resolves_prefixed_core_elements_and_escaped_attributes() {
        let resources = format!(
            "<metadata><vertex/></metadata>{}",
            mesh().replace("x=\"2\"", "x=\"&#50;\"")
        );
        let xml = model(&resources, "<item objectid=\"&#49;\"/>")
            .replace("<", "<c:")
            .replace("<c:/", "</c:")
            .replace("xmlns=", "xmlns:c=");
        let triangles = load_3mf(Cursor::new(package(&xml))).unwrap();
        assert_eq!(
            triangles,
            vec![[DVec3::ZERO, DVec3::new(2.0, 0.0, 0.0), DVec3::Y]]
        );
    }

    #[test]
    fn production_parts_resolve_scoped_ids_units_and_transforms() {
        let root = model(
            "<object id=\"1\"><components>
                <component objectid=\"1\" prod:path=\"Objects/a.model\" transform=\"2 0 0 0 2 0 0 0 2 3 0 0\"/>
                <component objectid=\"1\" prod:path=\"/3D/Objects/b.model\"/>
            </components></object>",
            "<item objectid=\"1\" transform=\"1 0 0 0 1 0 0 0 1 10 0 0\"/>
             <item objectid=\"1\" prod:path=\"Objects/a.model\"/>",
        ).replace("<model ", &format!("<model xmlns:prod=\"{PRODUCTION_NS}\" requiredextensions=\"prod\" "));
        let a = model(&mesh(), "");
        let b = model(&mesh(), "").replace("<model ", "<model unit=\"inch\" ");
        let parts = [
            ("3D/root.model", root.as_str()),
            ("3D/Objects/a.model", a.as_str()),
            ("3D/Objects/b.model", b.as_str()),
        ];
        let triangles = load_3mf(Cursor::new(package_parts(&parts))).unwrap();
        assert_eq!(triangles.len(), 3);
        assert_eq!(triangles[0][0], DVec3::new(13.0, 0.0, 0.0));
        assert_eq!(triangles[0][1], DVec3::new(17.0, 0.0, 0.0));
        assert_eq!(triangles[1][1], DVec3::new(60.8, 0.0, 0.0));
        assert_eq!(triangles[2][1], DVec3::new(2.0, 0.0, 0.0));
    }

    #[test]
    fn production_parts_reject_missing_references_cross_part_cycles_and_external_urls() {
        let root = model(
            "<object id=\"1\"><components><component objectid=\"1\" p:path=\"Objects/a.model\"/></components></object>",
            "<item objectid=\"1\"/>",
        ).replace("<model ", &format!("<model xmlns:p=\"{PRODUCTION_NS}\" requiredextensions=\"p\" "));
        assert!(load_3mf(Cursor::new(package_parts(&[("3D/root.model", &root)]))).is_err());
        let child = model(
            "<object id=\"1\"><components><component objectid=\"1\" p:path=\"../root.model\"/></components></object>",
            "",
        ).replace("<model ", &format!("<model xmlns:p=\"{PRODUCTION_NS}\" requiredextensions=\"p\" "));
        let error = load_3mf(Cursor::new(package_parts(&[
            ("3D/root.model", &root),
            ("3D/Objects/a.model", &child),
        ])))
        .unwrap_err();
        assert!(error.contains("cycle"));
        for target in ["../../../outside.model", "https://example.com/a.model", "/"] {
            let xml = root.replace("Objects/a.model", target);
            assert!(load_3mf(Cursor::new(package_parts(&[("3D/root.model", &xml)]))).is_err());
        }
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
            pan: egui::Vec2::ZERO,
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
    fn middle_drag_pan_translates_rendered_geometry_without_changing_shading() {
        let triangles = normalize(load_stl(ASCII_STL.as_bytes()).unwrap()).unwrap();
        let mut view = View {
            size: [80, 60],
            yaw: 0.0,
            pitch: 0.0,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
        };
        let initial = render(&triangles, view);
        view.pan = egui::vec2(0.1, -0.1);
        let panned = render(&triangles, view);
        for y in 0..60 {
            for x in 0..80 {
                let expected = if x >= 8 && y + 6 < 60 {
                    initial.pixels[(y + 6) * 80 + x - 8]
                } else {
                    BACKGROUND
                };
                assert_eq!(panned.pixels[y * 80 + x], expected);
            }
        }
        view.pan = egui::vec2(2.0, -2.0);
        assert!(
            render(&triangles, view)
                .pixels
                .iter()
                .all(|pixel| *pixel == BACKGROUND)
        );
    }

    #[test]
    fn pane_handles_scroll_left_button_rotation_and_middle_button_pan() {
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
                pan: egui::Vec2::ZERO,
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
        let unchanged = frame(vec![moved(60.0, 60.0)]);
        assert_eq!(unchanged.yaw, 0.0);
        assert_eq!(unchanged.pan, egui::Vec2::ZERO);
        frame(vec![button(
            egui::PointerButton::Secondary,
            false,
            60.0,
            60.0,
        )]);
        frame(vec![button(egui::PointerButton::Primary, true, 60.0, 60.0)]);
        let rotated = frame(vec![moved(100.0, 110.0)]);
        assert!(rotated.yaw > 0.0 && rotated.pitch > 0.0);
        assert_eq!(rotated.pan, egui::Vec2::ZERO);
        frame(vec![button(
            egui::PointerButton::Primary,
            false,
            100.0,
            110.0,
        )]);
        frame(vec![button(
            egui::PointerButton::Middle,
            true,
            100.0,
            110.0,
        )]);
        let panned = frame(vec![moved(70.0, 40.0)]);
        assert_eq!(panned.yaw, rotated.yaw);
        assert_eq!(panned.pitch, rotated.pitch);
        assert_eq!(panned.zoom, rotated.zoom);
        assert!((panned.pan.x * panned.size[0] as f32 + 30.0).abs() < 0.001);
        assert!((panned.pan.y * panned.size[1] as f32 + 70.0).abs() < 0.001);
        frame(vec![button(egui::PointerButton::Middle, false, 70.0, 40.0)]);
        assert_eq!(frame(vec![moved(90.0, 70.0)]).pan, panned.pan);
    }
}
