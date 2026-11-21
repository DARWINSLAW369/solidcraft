//! The operation and tool editor. Double-clicking an operation or a tool in the Browser, or its
//! Edit menu, opens it in a window with two tabs over one draft of it:
//! - **Fields** lays the parameters out as inputs: name, type, tool, the kind's own parameters,
//!   stock to leave, geometry, heights and linking. The form is built from the operation's own
//!   saved form and its kind's defaults, so every kind, present and future, gets one. A tool's
//!   form has its number, kind (with the kind's own sizes), size, and feeds and speeds.
//! - **JSON** shows the same draft as the text `cam.operation.set` (`cam.tool.set`) takes, for
//!   reading, copying and pasting.
//!
//! Switching tabs carries the edits across. Apply and OK run the set command with the draft, the
//! same command an assistant runs, as one undo step. When the operation or tool changes
//! elsewhere (an assistant, undo) an untouched draft follows it; an edited one offers to reload.

use std::collections::{BTreeSet, HashMap};

use egui::RichText;
use serde_json::{Map, Value, json};
use solidcraft_engine::cam;

use crate::SolveApp;
use solidcraft_theme::Tokens;

/// The editor's two tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Fields,
    Json,
}

/// Keys every operation has, beside its kind's parameters.
const COMMON: &[&str] = &[
    "id",
    "name",
    "enabled",
    "tool",
    "orientation",
    "geometry",
    "heights",
    "linking",
    "tolerance",
    "stock_to_leave_radial",
    "stock_to_leave_axial",
    "rest_machining",
    "boundary",
    "type",
];

/// What the editor edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Operation(u64),
    /// An operation not added yet, for this setup: OK and Apply add it (`cam.operation.add`).
    NewOperation(u64),
    Tool(u64),
    Setup(u64),
    /// A tool not added yet: OK and Apply add it (`cam.tool.add`).
    NewTool,
    /// A setup not created yet: OK and Apply create it (`cam.setup.create`).
    NewSetup,
    /// A command run with the form's parameters (Tool Kit, Set Machine): the draft is its
    /// parameters, each a choice from a list.
    Run(&'static str),
    /// Any command, its form built from its parameters ([`crate::auto_form`]).
    Command(&'static str),
}

impl Target {
    /// The command that sets it, and the parameter naming it (none for a new item or a plain
    /// command run).
    fn command(self) -> (&'static str, &'static str, u64) {
        match self {
            Target::Operation(id) => ("cam.operation.set", "operation", id),
            Target::NewOperation(setup) => ("cam.operation.add", "setup", setup),
            Target::Tool(id) => ("cam.tool.set", "tool", id),
            Target::Setup(id) => ("cam.setup.set", "setup", id),
            Target::NewTool => ("cam.tool.add", "", 0),
            Target::NewSetup => ("cam.setup.create", "", 0),
            Target::Run(command) | Target::Command(command) => (command, "", 0),
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Target::Operation(_) | Target::NewOperation(_) => "operation",
            Target::Tool(_) | Target::NewTool => "tool",
            Target::Setup(_) | Target::NewSetup => "setup",
            Target::Run(_) | Target::Command(_) => "command",
        }
    }

    /// Not saved in the design: the editor's own draft is all there is.
    fn unsaved(self) -> bool {
        matches!(self, Target::NewOperation(_) | Target::NewTool | Target::NewSetup | Target::Run(_) | Target::Command(_))
    }
}

/// The choices a plain command's form offers for each of its parameters (`setup` is a dropdown
/// of the setups, drawn apart): the parameter, its label and the values to pick from.
fn run_choices(app: &SolveApp, command: &str) -> Vec<(&'static str, &'static str, Vec<String>)> {
    let machines = || solidcraft_engine::cmd::cam::known_machines(&app.session).into_iter().map(|e| e.id).collect::<Vec<String>>();
    match command {
        "cam.tool.kit" => vec![
            ("kit", "Tool kit", cam::kits::KITS.iter().map(|k| k.to_string()).collect()),
            ("material", "Material", vec!["aluminium".into(), "wood".into(), "plastic".into()]),
        ],
        "cam.machine.set" => vec![("machine", "Machine", machines())],
        "cam.post" => vec![("post", "Post processor", cam::post::Post::BUILTIN.iter().map(|p| p.to_string()).collect())],
        _ => Vec::new(),
    }
}

/// The window title of a plain command's form.
fn run_title(command: &str) -> &'static str {
    match command {
        "cam.tool.kit" => "Add Tool Kit",
        "cam.machine.set" => "Set Machine",
        "cam.post" => "Post Program",
        "cam.simulate.show" | "cam.simulate" => "Simulate",
        _ => "Run Command",
    }
}

/// A tool kind's own sizes, beside the sizes every tool has.
const TOOL_KIND_KEYS: &[&str] = &[
    "corner_radius",
    "tip_angle_deg",
    "wavelength_nm",
    "tip_radius",
    "taper_angle_deg",
    "pierce_height",
    "cut_height",
    "pierce_delay_s",
    "angle_deg",
    "pitch",
    "round_radius",
    "tip_diameter",
];

/// The five heights, in the order they are met going down.
const HEIGHTS: [(&str, &str); 5] = [("clearance", "Clearance"), ("retract", "Retract"), ("feed", "Feed"), ("top", "Top"), ("bottom", "Bottom")];

/// The open editor.
pub struct CamEditor {
    pub target: Target,
    pub tab: Tab,
    /// The operation as it was opened or last applied, without its `id`.
    base: Value,
    /// The operation as edited.
    pub draft: Value,
    /// The JSON tab's text and, when it does not parse, why.
    pub text: String,
    pub text_error: Option<String>,
    /// The form's inputs.
    inputs: Inputs,
    /// The last Apply's outcome: a refusal (true) or the warnings it gave (false).
    pub message: Option<(String, bool)>,
    /// What the next Apply adds to `cam.setup.set`: `flip` (turn the setup over), `origin` (a
    /// coordinate or a construction point) or `origin_at` (a point of the stock or model).
    pub place: Option<Value>,
    /// Placing a setup's origin: the choices in the form, and picking on the model.
    pub origin: OriginForm,
    /// Folded down to its title and buttons, to see more of the model.
    pub minimized: bool,
    /// A command's own form ([`Target::Command`]): its fields.
    pub auto: Vec<crate::auto_form::AutoField>,
    /// The two-click setup (New Setup, Set Zero): which click comes next. `None`: the full form.
    pub quick: Option<Quick>,
    /// The selection when the top-face click was asked for, so the click that changes it is the one.
    quick_before: Option<Vec<solidcraft_engine::Sel>>,
    /// The setup's stock as last worked out, by (setup, design revision): working it out meshes
    /// every body of the setup (seconds on a big assembly), too slow for every frame.
    stock_cache: Option<((u64, u64), Option<(cam::setup::StockShape, cam::frame::Frame)>)>,
    /// Where the operation's Tool box takes its tools from: 0 the job's own, else a shop
    /// library (1 the first one listed).
    tool_from: usize,
    /// The shop library as `cam.library.list` gave it when the form opened.
    shop: Option<Value>,
    /// A shop library tool picked in the Tool box (its library index), copied into the job after
    /// the form is drawn.
    shop_pick: Option<usize>,
    /// A new operation made by clicking on the model (what to click, and what was clicked), the
    /// full form behind Adjust. `None`: the form.
    pub pick: Option<OpPick>,
}

/// What a click on the model picks for a new operation, in the words of what is seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickWhat {
    /// A level face: a pocket's floor, the top to face off.
    Floor,
    /// A wall or the edge along its top: the floor it stands on gives the outline and the depth.
    Walls,
    /// Holes, one click each.
    Holes,
    /// Faces to machine (3D), one click each.
    Faces,
    /// A sketch's shape.
    Sketch,
    /// One wall, its bottom and top edges the swarf chains.
    SwarfWall,
    /// Two edges, the rails of a flow or morph.
    Rails,
    /// Nothing to click: the operation works on the whole part (Go).
    Nothing,
}

/// Picking a new operation's geometry on the model.
pub struct OpPick {
    pub what: PickWhat,
    /// The selection when picking began or last changed, so the click that changes it is the one.
    before: Option<Vec<solidcraft_engine::Sel>>,
    /// The points clicked so far (holes and faces take several).
    points: Vec<Value>,
    /// A sketch clicked (its feature id).
    sketch: Option<u64>,
}

/// What can be clicked for an operation of kind `ty` (the first is asked for first), and the
/// button that takes the whole part or stock without a click. Empty: the form only.
fn pick_choices(ty: &str) -> (&'static [PickWhat], Option<&'static str>) {
    match ty {
        "face" => (&[PickWhat::Floor], Some("Whole Stock Top")),
        "pocket2d" | "adaptive2d" => (&[PickWhat::Floor, PickWhat::Walls], None),
        "contour2d" => (&[PickWhat::Walls, PickWhat::Floor], None),
        "chamfer2d" => (&[PickWhat::Walls], None),
        "slot" | "trace" | "engrave" | "laser_cut" | "laser_engrave" | "plasma_cut" => (&[PickWhat::Sketch], None),
        "drill" | "bore" | "circular" | "thread" => (&[PickWhat::Holes], None),
        "pocket3d" | "adaptive3d" | "parallel" | "contour3d" | "scallop" | "pencil" | "spiral" | "radial" | "morphed_spiral" | "project"
        | "horizontal" | "steep_shallow" | "multi_axis_contour" => (&[PickWhat::Faces], Some("Whole Part")),
        "swarf" => (&[PickWhat::SwarfWall], None),
        "flow" | "morph" => (&[PickWhat::Rails], None),
        "rotary" | "turn_face" | "turn_profile" | "turn_groove" | "turn_thread" => (&[PickWhat::Nothing], Some("Go")),
        _ => (&[], None),
    }
}

/// The line that says what to click.
fn pick_line(ty: &str, what: PickWhat) -> &'static str {
    match (what, ty) {
        (PickWhat::Floor, "face") => "Click the top face of the part, or take the whole stock top.",
        (PickWhat::Floor, "contour2d") => "Click the face the part stands on: its outline is cut.",
        (PickWhat::Floor, _) => "Click the floor of the pocket.",
        (PickWhat::Walls, "chamfer2d") => "Click the edge to chamfer (or the wall under it).",
        (PickWhat::Walls, "contour2d") => "Click a wall of the part, or the edge along its top.",
        (PickWhat::Walls, _) => "Click a wall of the pocket, or the edge along its top.",
        (PickWhat::Holes, _) => "Click each hole, then Done.",
        (PickWhat::Faces, _) => "Click the faces to machine, then Done, or take the whole part.",
        (PickWhat::Sketch, _) => "Click the sketch shape to cut.",
        (PickWhat::SwarfWall, _) => "Click the wall: the side of the tool runs along it, bottom edge to top edge.",
        (PickWhat::Rails, _) => "Click two edges: the passes run between them.",
        (PickWhat::Nothing, "turn_groove") => "Works on the whole part: pick the tool and Go, or Adjust to set where the groove goes.",
        (PickWhat::Nothing, "turn_thread") => "Works on the whole part: pick the tool and Go, or Adjust to set the pitch and length.",
        (PickWhat::Nothing, _) => "Works on the whole part: pick the tool and Go.",
    }
}

/// The button that switches to picking `what` instead.
fn pick_switch_label(what: PickWhat) -> &'static str {
    match what {
        PickWhat::Floor => "Pick the Floor Instead",
        PickWhat::Walls => "Pick a Wall or Edge Instead",
        PickWhat::Holes => "Pick Holes",
        PickWhat::Faces => "Pick Faces",
        PickWhat::Sketch => "Pick a Sketch",
        PickWhat::SwarfWall => "Pick a Wall",
        PickWhat::Rails => "Pick Two Edges",
        PickWhat::Nothing => "Whole Part",
    }
}

/// Whether `what` takes several clicks (then Done).
fn pick_many(what: PickWhat) -> bool {
    matches!(what, PickWhat::Holes | PickWhat::Faces)
}

/// What the pick panel asked for.
#[derive(Clone, Debug, PartialEq)]
enum PickAction {
    Switch(PickWhat),
    Whole,
    Done,
    UseSelected,
    Adjust,
}

/// The two clicks that set a setup up: the face that faces up, then the zero point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quick {
    Top,
    Zero,
}

/// Placing a setup's origin: which point of the stock or model, which construction point, and
/// picking on the model (the snap under the pointer, and the selection when picking began, so
/// the click that picks is the one that changes it).
#[derive(Default)]
pub struct OriginForm {
    of: usize,
    x: usize,
    y: usize,
    z: usize,
    point: usize,
    pub picking: bool,
    /// The stock is drawn while a setup is edited, unless this is set.
    pub hide_stock: bool,
    before: Option<Vec<solidcraft_engine::Sel>>,
    hover: Option<(solidcraft_engine::Sel, Option<crate::dialogs_assembly::SnapPt>)>,
    /// The snap under the pointer (of the model or the stock), and the one a click chose.
    current: Option<solidcore_record::Vec3>,
    chosen: Option<solidcore_record::Vec3>,
}

/// The text typed in each field (by the parameter's path), so a value half typed survives the
/// frame, and the fields whose text is not a value.
#[derive(Default)]
struct Inputs {
    text: HashMap<String, String>,
    invalid: BTreeSet<String>,
}

impl CamEditor {
    fn new(target: Target, saved: Value) -> CamEditor {
        CamEditor {
            target,
            tab: Tab::Fields,
            text: pretty(&saved),
            draft: saved.clone(),
            base: saved,
            text_error: None,
            inputs: Inputs::default(),
            message: None,
            place: None,
            origin: OriginForm::default(),
            minimized: false,
            auto: Vec::new(),
            quick: None,
            quick_before: None,
            stock_cache: None,
            tool_from: 0,
            shop: None,
            shop_pick: None,
            pick: None,
        }
    }

    /// Takes `v` as the draft, dropping what the fields held.
    fn set_draft(&mut self, v: Value) {
        self.text = pretty(&v);
        self.text_error = None;
        self.draft = v;
        self.inputs = Inputs::default();
    }

    /// Whether the draft differs from the operation.
    pub fn edited(&self) -> bool {
        match self.tab {
            Tab::Fields => self.draft != self.base,
            Tab::Json => serde_json::from_str::<Value>(&self.text).map_or(true, |v| v != self.base),
        }
    }

    /// Why Apply cannot run yet, if it cannot.
    pub fn blocked(&self) -> Option<String> {
        match self.tab {
            Tab::Json => self.text_error.clone(),
            Tab::Fields => self.inputs.invalid.first().map(|p| format!("`{p}` is not a valid value")),
        }
    }
}

/// The operation geometry the viewport's selection makes for an operation of kind `ty`, and a few
/// words saying what it is: picked faces (holes for drilling kinds) or a sketch's profiles.
pub fn selection_geometry(app: &SolveApp, ty: &str) -> Option<(Value, String)> {
    use solidcraft_engine::Sel;
    let sel = &app.session.selection;
    let points: Vec<Value> =
        sel.iter().filter_map(|s| if let Sel::Face { point, .. } = s { Some(json!([point.x, point.y, point.z])) } else { None }).collect();
    if !points.is_empty() {
        let n = points.len();
        let holes = matches!(ty, "drill" | "bore" | "circular" | "thread");
        let (g, what) = if holes {
            (json!({"source": "holes", "points": points, "diameter": 0.0}), if n == 1 { "1 hole".to_string() } else { format!("{n} holes") })
        } else {
            (json!({"source": "faces", "points": points}), if n == 1 { "1 face".to_string() } else { format!("{n} faces") })
        };
        return Some((g, what));
    }
    sel.iter().find_map(|s| if let Sel::Profile { sketch, .. } = s { Some(*sketch) } else { None }).map(|feature| {
        let name = app.session.model.state().sketch(feature).map(|k| k.name.clone()).unwrap_or_else(|| format!("sketch {feature}"));
        (json!({"source": "sketch", "feature": feature}), format!("the profiles of {name}"))
    })
}

/// The operation or tool as saved, without its `id`.
fn saved(app: &SolveApp, target: Target) -> Option<Value> {
    let m = &app.session.doc.manufacture;
    let mut v = match target {
        Target::Operation(op) => serde_json::to_value(m.operation(op)?.1).ok()?,
        Target::Tool(tool) => serde_json::to_value(m.tools.get(tool)?).ok()?,
        Target::Setup(setup) => serde_json::to_value(m.setup(setup)?).ok()?,
        Target::NewOperation(_) | Target::NewTool | Target::NewSetup | Target::Run(_) | Target::Command(_) => return None,
    };
    if let Value::Object(m) = &mut v {
        m.remove("id");
        // A setup's operations are edited one by one.
        if matches!(target, Target::Setup(_)) {
            m.remove("operations");
        }
    }
    Some(v)
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// Opens the editor on an operation or a tool (opening another replaces it).
pub fn open(app: &mut SolveApp, target: Target) {
    if let Some(v) = saved(app, target) {
        app.cam_editor = Some(CamEditor::new(target, v));
    }
}

/// Opens the editor on a new operation for `setup`, from a `cam.operation.<kind>` call (its
/// kind's parameters, tool, name and geometry): the whole operation is in the form before it is
/// added. False when the call does not make an operation.
pub fn open_new(app: &mut SolveApp, setup: u64, kind: &str, call: &Value) -> bool {
    let mut v = call.clone();
    let Value::Object(o) = &mut v else { return false };
    o.remove("setup");
    o.entry("type").or_insert_with(|| json!(kind));
    o.insert("id".into(), json!(0));
    // Through the saved form, so every field the operation has is in the draft.
    let Some(mut full) = serde_json::from_value::<cam::setup::Operation>(v).ok().and_then(|op| serde_json::to_value(op).ok()) else {
        return false;
    };
    if let Value::Object(m) = &mut full {
        m.remove("id");
    }
    let mut ed = CamEditor::new(Target::NewOperation(setup), full);
    // Click on the model first, the form behind Adjust; something already selected fills the
    // form instead (picked before the operation was asked for).
    if let Some(first) = pick_choices(kind).0.first()
        && selection_geometry(app, kind).is_none()
    {
        ed.pick = Some(OpPick { what: *first, before: Some(app.session.selection.clone()), points: Vec::new(), sketch: None });
        app.set_status(solidcraft_ui_text::tr_label(pick_line(kind, *first)).to_string(), false);
    }
    app.cam_editor = Some(ed);
    true
}

/// Opens the editor on a new tool: a 6 mm flat end mill numbered after the library's highest,
/// every field in the form before it is added.
pub fn open_new_tool(app: &mut SolveApp) {
    let number = app.session.doc.manufacture.tools.tools.iter().map(|t| t.number).max().unwrap_or(0) + 1;
    let mut tool = cam::tool::Tool::new(0, "6 mm flat", cam::tool::ToolKind::FlatEnd, 6.0);
    tool.number = number.min(9999);
    let Ok(mut v) = serde_json::to_value(&tool) else { return };
    if let Value::Object(m) = &mut v {
        m.remove("id");
    }
    app.cam_editor = Some(CamEditor::new(Target::NewTool, v));
}

/// Opens the editor on a new setup, as `cam.setup.create` would make it with no parameters: the
/// first measured from the design's origin with a box of stock round the model, a later one
/// carrying the setup before's stock and machine over.
pub fn open_new_setup(app: &mut SolveApp) {
    let m = &app.session.doc.manufacture;
    let before = m.setups.last();
    let setup = cam::setup::Setup {
        id: 0,
        name: format!("Setup {}", m.setups.len() + 1),
        bodies: Vec::new(),
        fixtures: Vec::new(),
        wcs: cam::frame::Frame::IDENTITY,
        stock: if before.is_some() { cam::setup::Stock::FromSetup { setup: 0 } } else { cam::setup::Stock::default() },
        machine: before.map_or_else(
            || {
                let id = &app.preferences.default_machine;
                solidcraft_engine::cmd::cam::known_machines(&app.session)
                    .into_iter()
                    .find(|e| !id.is_empty() && e.id == *id)
                    .map_or_else(cam::machine::Machine::three_axis, |e| e.machine)
            },
            |b| b.machine.clone(),
        ),
        operations: Vec::new(),
        program: String::new(),
    };
    let Ok(mut v) = serde_json::to_value(&setup) else { return };
    if let Value::Object(o) = &mut v {
        o.remove("id");
        o.remove("operations");
    }
    app.cam_editor = Some(CamEditor::new(Target::NewSetup, v));
}

/// The stock margin a new setup starts with, on every side (the model dead centre), mm.
const QUICK_MARGIN: f64 = 1.0;

/// A box stock `margin` mm bigger than the model on every side, the model dead centre.
fn margin_stock(margin: f64) -> Value {
    json!({"mode": "relative_box", "x_neg": margin, "x_pos": margin, "y_neg": margin, "y_pos": margin, "z_neg": margin, "z_pos": margin})
}

/// New Setup and Set Zero: two clicks, no form. `setup`: the setup to zero; `None` makes a new
/// one first (a box stock 1 mm around the model on every side; a later setup carries the one
/// before's stock over). Then: click the face that faces up, then the zero point (a corner, edge
/// or hole of the model or the stock).
pub fn open_quick_setup(app: &mut SolveApp, setup: Option<u64>) {
    let id = match setup {
        Some(id) => id,
        None => {
            let first = app.session.doc.manufacture.setups.is_empty();
            let mut params = if first { json!({"stock": margin_stock(QUICK_MARGIN)}) } else { json!({}) };
            // The first setup starts on the machine Preferences names (a later one carries the
            // setup before's over).
            let default_machine = app.preferences.default_machine.clone();
            if first && !default_machine.is_empty() {
                params["machine"] = json!(default_machine);
            }
            match app.run("cam.setup.create", params) {
                Ok(v) => match v["id"].as_u64() {
                    Some(id) => id,
                    None => return,
                },
                Err(e) => {
                    app.set_status(e, true);
                    return;
                }
            }
        }
    };
    let Some(saved_setup) = saved(app, Target::Setup(id)) else { return };
    let mut ed = CamEditor::new(Target::Setup(id), saved_setup);
    ed.quick = Some(Quick::Top);
    ed.quick_before = Some(app.session.selection.clone());
    app.cam_editor = Some(ed);
    app.set_status("Click the face that faces up (the top of the part), or Keep Z Up.".to_string(), false);
}

/// The setup's axes turned so `z` (a picked face's normal) is up, the origin kept: x is the
/// design's X laid into the new XY plane (its Y where X is too steep).
fn wcs_up(app: &SolveApp, setup: u64, z: solidcore_record::Vec3) -> Option<Value> {
    let st = app.session.doc.manufacture.setup(setup)?;
    let n = (z.x * z.x + z.y * z.y + z.z * z.z).sqrt();
    if n < 1e-9 {
        return None;
    }
    let z = [z.x / n, z.y / n, z.z / n];
    let lay = |a: [f64; 3]| {
        let d = a[0] * z[0] + a[1] * z[1] + a[2] * z[2];
        [a[0] - d * z[0], a[1] - d * z[1], a[2] - d * z[2]]
    };
    let len = |a: [f64; 3]| (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    let mut x = lay([1.0, 0.0, 0.0]);
    if len(x) < 0.3 {
        x = lay([0.0, 1.0, 0.0]);
    }
    let o = st.wcs.origin;
    Some(json!({"origin": [o.x, o.y, o.z], "z": z, "x": x}))
}

/// The two-click panel: what to click next, the stock margin, Keep Z Up, Advanced and Done.
/// Returns what was pressed.
fn quick_panel(ui: &mut egui::Ui, ed: &mut CamEditor, step: Quick) -> Option<QuickAction> {
    let t = Tokens::get();
    let mut out = None;
    let text = match step {
        Quick::Top => "1. Click the face that faces up: the top of the part.",
        Quick::Zero => "2. Click your zero: a corner, edge middle, face centre or hole of the model or the stock.",
    };
    ui.label(RichText::new(solidcraft_ui_text::tr_label(text)).strong().color(t.accent));
    ui.add_space(t.space_8);
    // The margin, when the stock is a box around the model (the model dead centre).
    let relative = ed.draft["stock"]["mode"].as_str() == Some("relative_box");
    if relative {
        let now = ed.draft["stock"]["x_neg"].as_f64().unwrap_or(QUICK_MARGIN);
        let shown = number_text(&json!(now));
        if let Some(v) = typed(ui, &mut ed.inputs, "quick.margin", "Stock margin, every side (mm)", &shown, "", |s| {
            parse_number(s, false).filter(|v| v.as_f64().is_some_and(|x| (0.0..=1000.0).contains(&x)))
        }) && let Some(m) = v.as_f64()
        {
            out = Some(QuickAction::Margin(m));
        }
    }
    ui.add_space(t.space_8);
    ui.horizontal(|ui| {
        if step == Quick::Top && crate::ui_kit::button(ui, solidcraft_ui_text::tr_label("Keep Z Up"), crate::ui_kit::Button::Normal).clicked() {
            out = Some(QuickAction::KeepZ);
        }
        if crate::ui_kit::button(ui, solidcraft_ui_text::tr_label("Advanced…"), crate::ui_kit::Button::Normal)
            .on_hover_text(solidcraft_ui_text::tr_label("The full setup form: each side of the stock, fixed sizes, round stock, the machine"))
            .clicked()
        {
            out = Some(QuickAction::Advanced);
        }
    });
    out
}

/// A new operation's click panel: what to click, the tool, the other things it can pick, and
/// Adjust for the full form. Returns what was pressed.
fn pick_panel(
    ui: &mut egui::Ui,
    pick: &OpPick,
    draft: &mut Value,
    inputs: &mut Inputs,
    hover: Option<crate::toolbar::HoverShape>,
    tools: &[(u64, String)],
    from: &mut ToolFrom,
    selected: Option<&str>,
) -> Option<PickAction> {
    let t = Tokens::get();
    let mut out = None;
    let ty = draft["type"].as_str().unwrap_or("").to_string();
    let (choices, whole) = pick_choices(&ty);
    ui.label(RichText::new(solidcraft_ui_text::tr_label(pick_line(&ty, pick.what))).strong().color(t.accent));
    if pick_many(pick.what) && !pick.points.is_empty() {
        let n = pick.points.len();
        let noun = if pick.what == PickWhat::Holes { "hole" } else { "face" };
        ui.label(RichText::new(format!("{n} {noun}{} picked.", if n == 1 { "" } else { "s" })).color(t.text_dim));
    }
    if pick.what == PickWhat::Rails && pick.points.len() == 1 {
        ui.label(RichText::new("1 of 2 edges picked: click the second.").color(t.text_dim));
    }
    ui.add_space(t.space_8);
    if let Value::Object(op) = draft {
        tool_rows(ui, op, tools, from);
    }
    if tools.is_empty() && *from.from == 0 {
        ui.label(RichText::new("No tools in this job yet: pick one from a library in Tool from.").color(t.warning));
    }
    // A chamfer needs a chamfer mill (or an engraver): say so when the tool picked is not one.
    if matches!(ty.as_str(), "chamfer2d") {
        let tool = draft["tool"].as_u64().unwrap_or(0);
        let label = tools.iter().find(|x| x.0 == tool).map(|x| x.1.to_ascii_lowercase()).unwrap_or_default();
        if !(label.contains("chamfer") || label.contains("engrav")) {
            ui.label(RichText::new("A chamfer needs a chamfer mill: pick one in Tool (or from a library in Tool from).").color(t.warning));
        }
    }
    ui.add_space(t.space_8);
    ui.horizontal_wrapped(|ui| {
        if pick_many(pick.what)
            && !pick.points.is_empty()
            && crate::ui_kit::button(ui, solidcraft_ui_text::tr_label("Done"), crate::ui_kit::Button::Primary).clicked()
        {
            out = Some(PickAction::Done);
        }
        if let Some(w) = whole
            && crate::ui_kit::button(ui, solidcraft_ui_text::tr_label(w), crate::ui_kit::Button::Normal).clicked()
        {
            out = Some(PickAction::Whole);
        }
        for c in choices.iter().filter(|c| **c != pick.what) {
            if crate::ui_kit::button(ui, solidcraft_ui_text::tr_label(pick_switch_label(*c)), crate::ui_kit::Button::Normal).clicked() {
                out = Some(PickAction::Switch(*c));
            }
        }
        if let Some(what) = selected
            && crate::ui_kit::button(ui, format!("Use What's Selected ({what})"), crate::ui_kit::Button::Normal).clicked()
        {
            out = Some(PickAction::UseSelected);
        }
        if crate::ui_kit::button(ui, solidcraft_ui_text::tr_label("Adjust…"), crate::ui_kit::Button::Normal)
            .on_hover_text(solidcraft_ui_text::tr_label("The full form: depths, stepover, heights, linking and the rest"))
            .clicked()
        {
            out = Some(PickAction::Adjust);
        }
    });
    // Below the buttons (so they stay put while it changes): the few settings that belong to
    // what the pointer is on in the model, the basics when it is on nothing; where those numbers
    // live, and changed right here.
    if let Value::Object(op) = draft {
        quick_rows(ui, op, inputs, hover);
    }
    out
}

/// The settings the click panel shows for what the pointer is on: (the key, under `heights`
/// when it starts with `heights.`, and its label). Only those the operation has are drawn.
fn quick_keys(ty: &str, hover: Option<crate::toolbar::HoverShape>) -> Vec<(&'static str, &'static str)> {
    use crate::toolbar::HoverShape as H;
    let holes = matches!(ty, "drill" | "bore" | "circular" | "thread");
    match hover {
        _ if holes => {
            vec![("heights.bottom", "Hole bottom"), ("break_through", "Break-through (mm)"), ("peck", "Peck depth (mm)"), ("cycle", "Cycle")]
        }
        Some(H::Top) => vec![("heights.top", "Top"), ("heights.bottom", "Bottom: how deep it cuts"), ("stepdown", "Stepdown (mm)"), ("stepover", "Stepover (mm)")],
        Some(H::Floor) => vec![("heights.bottom", "Bottom: how deep it cuts"), ("heights.top", "Top"), ("stepdown", "Stepdown (mm)"), ("stock_to_leave_axial", "Leave on the floor (mm)")],
        Some(H::Wall | H::OutsideWall | H::Boss) => {
            vec![("stock_to_leave_radial", "Leave on the walls (mm)"), ("side", "Side"), ("stepover", "Stepover (mm)"), ("stepdown", "Stepdown (mm)")]
        }
        Some(H::Edge | H::OutsideEdge) if ty == "chamfer2d" => vec![("width", "Chamfer width (mm)"), ("tip_offset", "Tip below the edge (mm)"), ("side", "Side")],
        Some(H::Edge | H::OutsideEdge) => vec![("stock_to_leave_radial", "Leave on the walls (mm)"), ("side", "Side"), ("heights.top", "Top")],
        Some(H::Curved) => vec![("stepover", "Stepover (mm)"), ("stock_to_leave_axial", "Leave on the surface (mm)"), ("tolerance", "Tolerance (mm)")],
        Some(H::Hole) | None => vec![
            ("heights.bottom", "Bottom: how deep it cuts"),
            ("stepdown", "Stepdown (mm)"),
            ("stepover", "Stepover (mm)"),
            ("width", "Width (mm)"),
            ("stock_to_leave_radial", "Leave on the walls (mm)"),
        ],
    }
}

/// The click panel's few settings for what the pointer is on.
fn quick_rows(ui: &mut egui::Ui, op: &mut Map<String, Value>, inputs: &mut Inputs, hover: Option<crate::toolbar::HoverShape>) {
    let t = Tokens::get();
    let ty = op.get("type").and_then(Value::as_str).unwrap_or("").to_string();
    let keys: Vec<(&str, &str)> = quick_keys(&ty, hover)
        .into_iter()
        .filter(|(k, _)| match k.strip_prefix("heights.") {
            Some(h) => op.get("heights").and_then(|x| x.get(h)).is_some(),
            None => op.contains_key(*k),
        })
        .take(4)
        .collect();
    if keys.is_empty() {
        return;
    }
    let title = match hover {
        Some(crate::toolbar::HoverShape::Top) => "Settings for the top of the part under the pointer",
        Some(crate::toolbar::HoverShape::OutsideEdge) => "Settings for the outside edge under the pointer",
        Some(crate::toolbar::HoverShape::Boss) => "Settings for the round boss under the pointer",
        Some(crate::toolbar::HoverShape::Floor) => "Settings for the floor under the pointer",
        Some(crate::toolbar::HoverShape::Wall) => "Settings for the wall under the pointer",
        Some(crate::toolbar::HoverShape::OutsideWall) => "Settings for the outside wall under the pointer",
        Some(crate::toolbar::HoverShape::Edge) => "Settings for the edge under the pointer",
        Some(crate::toolbar::HoverShape::Hole) => "Settings for the hole under the pointer",
        Some(crate::toolbar::HoverShape::Curved) => "Settings for the surface under the pointer",
        None => "Main settings (point at the model to see where its numbers live)",
    };
    ui.add_space(t.space_8);
    ui.label(RichText::new(solidcraft_ui_text::tr_label(title)).color(t.text_dim));
    let choices = cam::ops::OpKind::choices(&ty);
    let from = cam::ops::variants::<cam::linking::HeightRef>(Value::Null);
    for (key, label) in keys {
        // Preferences ▸ Names: the hover box says it in the chosen words, as the full form does.
        let label = solidcraft_ui_text::names::cam(label);
        match key.strip_prefix("heights.") {
            Some(h) => {
                if let Some(Value::Object(hs)) = op.get_mut("heights")
                    && let Some(v) = hs.get_mut(h)
                {
                    height(ui, inputs, key, label, v, &from);
                }
            }
            None => {
                let list = choices.iter().find(|(k, _)| k == key).map(|x| x.1.as_slice());
                if let Some(v) = op.get_mut(key) {
                    value(ui, inputs, key, label, v, list);
                }
            }
        }
    }
}

/// Whether `g` is the blank geometry a new operation opens with (a point at the origin, sketch
/// 0, the made-up chains and rails), not anything picked.
fn placeholder_geometry(g: &Value) -> bool {
    let origin = |p: &Value| p.as_array().is_some_and(|a| a.iter().all(|x| x.as_f64() == Some(0.0)));
    match g["source"].as_str() {
        Some("faces" | "holes" | "walls") => g["points"].as_array().is_some_and(|a| a.is_empty() || a.iter().all(origin)),
        Some("sketch") => g["feature"].as_u64() == Some(0),
        Some("chains") => g["bottom"] == json!([[0, 0, 0], [10, 0, 0]]) || g["bottom"] == json!([[0.0, 0.0, 0.0], [10.0, 0.0, 0.0]]),
        Some("rails") => g["first"] == json!([[0, 0, 0], [10, 0, 0]]) || g["first"] == json!([[0.0, 0.0, 0.0], [10.0, 0.0, 0.0]]),
        _ => false,
    }
}

/// The geometry a pick makes for an operation of kind `ty`.
fn pick_geometry(what: PickWhat, points: &[Value], sketch: Option<u64>) -> Option<Value> {
    if points.is_empty() && sketch.is_none() {
        return None;
    }
    Some(match what {
        // A floor and a wall both find the floor (a wall clicked by mistake stands on it).
        PickWhat::Floor | PickWhat::Walls => json!({"source": "walls", "points": points}),
        PickWhat::Faces => json!({"source": "faces", "points": points}),
        PickWhat::Holes => json!({"source": "holes", "points": points, "diameter": 0.0}),
        PickWhat::Sketch => json!({"source": "sketch", "feature": sketch?}),
        PickWhat::SwarfWall => json!({"source": "swarf_wall", "point": points.first()?}),
        PickWhat::Rails if points.len() == 2 => json!({"source": "rail_edges", "points": points}),
        PickWhat::Rails | PickWhat::Nothing => return None,
    })
}

/// What one click picked for `what`: a point on the face, edge or hole, or a sketch.
fn pick_of(sel: &solidcraft_engine::Sel, what: PickWhat) -> Option<(Option<Value>, Option<u64>)> {
    use solidcraft_engine::Sel;
    match (sel, what) {
        (Sel::Profile { sketch, .. }, PickWhat::Sketch) => Some((None, Some(*sketch))),
        (Sel::Face { point, .. }, PickWhat::Floor | PickWhat::Faces | PickWhat::Walls | PickWhat::Holes | PickWhat::SwarfWall)
        | (Sel::Edge { point, .. }, PickWhat::Walls | PickWhat::Holes | PickWhat::SwarfWall | PickWhat::Rails) => {
            Some((Some(json!([point.x, point.y, point.z])), None))
        }
        _ => None,
    }
}

/// Puts what was picked into the new operation, adds it and makes its toolpath; the editor
/// closes when it is added. False when it was refused (the message says why).
fn finish_pick(app: &mut SolveApp, ed: &mut CamEditor, geometry: Option<Value>) -> bool {
    let ty = ed.draft["type"].as_str().unwrap_or("").to_string();
    if let (Some(g), Value::Object(op)) = (geometry, &mut ed.draft) {
        op.insert("geometry".into(), g);
        // An area operation keeps to what was picked.
        if serde_json::from_value::<cam::ops::OpKind>(json!({"type": ty})).is_ok_and(|k| k.uses_boundary()) {
            let b = op.entry("boundary").or_insert_with(|| serde_json::to_value(cam::boundary::Boundary::default()).unwrap_or(Value::Null));
            if let Value::Object(b) = b {
                b.insert("source".into(), json!("selection"));
            }
        }
    }
    if !apply(app, ed) {
        return false;
    }
    let Target::Operation(id) = ed.target else { return false };
    let name = ed.draft["name"].as_str().unwrap_or("Operation").to_string();
    let _ = app.session.execute("select.clear", &json!({}));
    // `apply` made its toolpath.
    if app.session.cam.operations.contains_key(&id) {
        app.set_status(format!("{name} added and its toolpath made. Double-click it in the Browser to adjust it."), false);
    } else {
        let why = ed.message.as_ref().map(|m| m.0.clone()).unwrap_or_default();
        app.set_status(format!("{name} added; its toolpath could not be made. {why} Double-click it in the Browser to adjust it."), true);
    }
    true
}

/// What the two-click panel asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum QuickAction {
    Margin(f64),
    KeepZ,
    Advanced,
}

/// Opens a plain command's form (Tool Kit, Set Machine) with these starting parameters.
pub fn open_run(app: &mut SolveApp, command: &'static str, params: Value) {
    app.cam_editor = Some(CamEditor::new(Target::Run(command), params));
}

/// Opens `id`'s own form, built from its parameters ([`crate::auto_form`]), with `prefill` and the
/// selected bodies filled in. False when the command has no parameter schema to build it from.
pub fn open_command(app: &mut SolveApp, id: &str, prefill: Value) -> bool {
    let Some(spec) = solidcraft_engine::find_command(id) else { return false };
    let Some(fields) = crate::auto_form::fields(id) else { return false };
    let mut draft = match prefill {
        Value::Object(m) => Value::Object(m),
        _ => json!({}),
    };
    let selected: Vec<String> = app
        .session
        .selection
        .iter()
        .filter_map(|s| match s {
            solidcraft_engine::Sel::Body { name } => Some(name.clone()),
            _ => None,
        })
        .collect();
    for f in &fields {
        if draft.get(&f.key).is_some() {
            continue;
        }
        match f.kind {
            crate::auto_form::AutoKind::Body => {
                if let Some(b) = selected.first() {
                    draft[f.key.as_str()] = json!(b);
                }
            }
            crate::auto_form::AutoKind::Bodies if !selected.is_empty() => draft[f.key.as_str()] = json!(selected),
            _ => {}
        }
    }
    let mut ed = CamEditor::new(Target::Command(spec.id), draft);
    ed.auto = fields;
    app.cam_editor = Some(ed);
    true
}

/// The post a setup's program is written with when none is chosen: its machine's own, else
/// the generic 3- or multi-axis one (as `cam.post` decides).
fn default_post(st: &cam::setup::Setup) -> String {
    if !st.machine.post.is_empty() {
        st.machine.post.clone()
    } else if st.machine.axis_count() > 3 {
        "fiveaxis".into()
    } else {
        "generic".into()
    }
}

/// Opens the Post form for `setup` (else the first setup): the post (the machine's own picked),
/// the program name, line numbers and canned cycles. OK then asks where to save the file.
pub fn open_post(app: &mut SolveApp, setup: Option<u64>) {
    let m = &app.session.doc.manufacture;
    let Some(st) = setup.and_then(|s| m.setup(s)).or_else(|| m.setups.first()) else {
        app.set_status("Create a setup first: there is nothing to post.", true);
        return;
    };
    let program = if st.program.is_empty() { st.name.clone() } else { st.program.clone() };
    let params = json!({
        "setup": st.id,
        "post": default_post(st),
        "program": program,
        "line_numbers": 0,
        "canned_cycles": true,
        "group_by_tool": st.machine.tool_changer == cam::machine::ToolChanger::Manual,
    });
    app.cam_editor = Some(CamEditor::new(Target::Run("cam.post"), params));
}

/// Post form OK: asks where to save, with the post's own extension (Mach3: .tap), then posts.
fn post_to_file(app: &mut SolveApp, params: Value) {
    let post_id = params["post"].as_str().unwrap_or("generic").to_string();
    let ext = cam::post::Post::builtin(&post_id).map(|p| p.extension).unwrap_or_else(|| "nc".into());
    let name: String = params["program"].as_str().unwrap_or("program").chars().filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
    let name = if name.trim().is_empty() { "program".to_string() } else { name };
    let mut exts: Vec<&str> = vec!["tap", "nc", "gcode", "ngc", "cnc"];
    exts.retain(|e| *e != ext);
    exts.insert(0, ext.as_str());
    app.pick_save_command(&format!("{name}.{ext}"), &exts, "cam.post", params);
}

/// Runs the set command with the draft; true when it was taken.
pub fn apply(app: &mut SolveApp, ed: &mut CamEditor) -> bool {
    if ed.tab == Tab::Json {
        match serde_json::from_str::<Value>(&ed.text) {
            Ok(v) => ed.draft = v,
            Err(e) => {
                ed.text_error = Some(e.to_string());
                return false;
            }
        }
    }
    if let Some(why) = ed.blocked() {
        ed.message = Some((why, true));
        return false;
    }
    let mut params = ed.draft.clone();
    if !params.is_object() {
        ed.message = Some((format!("the {} must be a JSON object", ed.target.noun()), true));
        return false;
    }
    // A new operation with nothing picked on the model (its geometry still the blank it opened
    // with) would only fail: say what to click instead of adding it.
    if matches!(ed.target, Target::NewOperation(_)) && params.get("geometry").is_some_and(placeholder_geometry) {
        let ty = params["type"].as_str().unwrap_or("").to_string();
        let what = pick_choices(&ty).0.first().map_or("Pick what to cut on the model first.", |w| pick_line(&ty, *w));
        ed.message = Some((format!("Nothing picked yet. {what} (Pick on the Model)"), true));
        return false;
    }
    let (command, key, id) = ed.target.command();
    if !key.is_empty() {
        params[key] = json!(id);
    }
    if let Some(Value::Object(extra)) = ed.place.take() {
        for (k, v) in extra {
            params[k.as_str()] = v;
        }
    }
    if let Value::Object(m) = &mut params {
        // A command's form: a parameter left empty takes the command's default.
        if matches!(ed.target, Target::Command(_)) {
            m.retain(|_, v| !v.is_null());
            if let Some(f) = ed.auto.iter().find(|f| f.required && !m.contains_key(&f.key)) {
                ed.message = Some((format!("Fill in {} first.", f.label), true));
                return false;
            }
        } else {
            m.remove("id");
        }
    }
    // Post: the file comes next (the save window), and the post runs once it is chosen.
    if command == "cam.post" {
        post_to_file(app, params);
        return true;
    }
    match app.run(command, params) {
        Ok(v) => {
            // Added: from now on the editor edits the operation, tool or setup it made.
            match (ed.target, v["id"].as_u64()) {
                (Target::NewOperation(_), Some(id)) => ed.target = Target::Operation(id),
                (Target::NewTool, Some(id)) => ed.target = Target::Tool(id),
                (Target::NewSetup, Some(id)) => ed.target = Target::Setup(id),
                _ => {}
            }
            let mut warnings: Vec<String> =
                v["notice"]["issues"].as_array().into_iter().flatten().filter_map(|i| i["message"].as_str().map(str::to_string)).collect();
            // An operation added or changed gets its toolpath at once (nothing waits on Generate).
            if let Target::Operation(id) = ed.target {
                match app.run("cam.generate", json!({"operation": id})) {
                    Err(e) => warnings.push(format!("Its toolpath could not be made: {e}")),
                    Ok(_) if !app.session.cam.operations.contains_key(&id) => {
                        warnings.push("Its toolpath could not be made: see the operation's notice in the Browser.".into())
                    }
                    Ok(_) => {}
                }
            }
            // A plain command says what it did (Tool Kit: the tools it added).
            if let (Target::Run(_) | Target::Command(_), Some(info)) = (ed.target, v["notice"]["info"].as_str()) {
                warnings.insert(0, info.to_string());
            }
            if let Some(now) = saved(app, ed.target) {
                ed.base = now.clone();
                ed.set_draft(now);
            }
            ed.message = (!warnings.is_empty()).then(|| (warnings.join("\n"), false));
            true
        }
        // Repair Part on a part that is already good: good news, not an error.
        Err(e) if e.contains("nothing to repair") => {
            // Replaces the red notice the refusal left in the status bar.
            app.set_status("Repair Part: the part is already good, nothing to repair.", false);
            ed.message = Some((format!("{GOOD}Good news: this part has no gaps or holes, nothing to repair."), false));
            false
        }
        Err(e) => {
            ed.message = Some((e, true));
            false
        }
    }
}

/// A form message that starts with this is good news, drawn in the success colour.
const GOOD: &str = "✓ ";

/// Draws the editor when it is open. Called once per frame.
pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut ed) = app.cam_editor.take() else { return };
    // Deleted (or undone away): nothing left to edit.
    let now = match ed.target {
        target if target.unsaved() => ed.base.clone(),
        target => match saved(app, target) {
            Some(v) => v,
            None => return,
        },
    };
    let mut changed_elsewhere = false;
    if now != ed.base {
        if ed.edited() {
            changed_elsewhere = true;
        } else {
            ed.base = now.clone();
            ed.set_draft(now.clone());
        }
    }
    let t = Tokens::get();
    let name = match (ed.base["name"].as_str(), ed.target) {
        (Some(n), Target::Tool(_)) => format!("T{} {n}", ed.base["number"].as_u64().unwrap_or(0)),
        (Some(n), Target::Operation(_)) => n.to_string(),
        (Some(n), Target::NewOperation(_)) => format!("New operation: {n}"),
        (Some(n), Target::Setup(_)) => format!("Setup: {n}"),
        (Some(n), Target::NewTool) => format!("New tool: {n}"),
        (Some(n), Target::NewSetup) => format!("New setup: {n}"),
        (_, Target::Run(command)) => run_title(command).to_string(),
        (_, Target::Command(command)) => {
            solidcraft_engine::find_command(command).map_or_else(|| command.to_string(), |s| solidcraft_ui_text::tr_label(s.label).to_string())
        }
        (None, _) => kind_label(ed.target.noun()),
    };
    let run_fields = match ed.target {
        Target::Run(command) => run_choices(app, command),
        _ => Vec::new(),
    };
    let uses = match ed.target {
        Target::Tool(id) => app.session.doc.manufacture.tool_uses(id),
        _ => 0,
    };
    let setups: Vec<(u64, String)> = app.session.doc.manufacture.setups.iter().map(|x| (x.id, x.name.clone())).collect();
    // The boundary the last generation used, from its notice.
    let used: Option<String> = match ed.target {
        Target::Operation(id) => {
            app.session.cam.operations.get(&id).and_then(|g| g.notice.issues.iter().find(|i| i.code == "boundary").map(|i| i.message.clone()))
        }
        _ => None,
    };
    // The job's tools by T number, each with its kind.
    let mut job: Vec<&cam::tool::Tool> = app.session.doc.manufacture.tools.tools.iter().collect();
    job.sort_by_key(|tl| tl.number);
    let tools: Vec<(u64, String)> = job.iter().map(|tl| (tl.id, format!("T{}  ·  {}  ·  {}", tl.number, tl.name, tl.kind.label()))).collect();
    // The shop libraries, for the Tool box: read once when an operation's form opens.
    let is_op = matches!(ed.target, Target::Operation(_) | Target::NewOperation(_));
    if is_op && ed.shop.is_none() {
        ed.shop = Some(app.run("cam.library.list", json!({})).unwrap_or(Value::Null));
    }
    let libraries: Vec<(String, Vec<(usize, String)>)> = if is_op { shop_libraries(ed.shop.as_ref()) } else { Vec::new() };
    let machines: Vec<(String, cam::machine::Machine)> = match ed.target {
        Target::Setup(_) | Target::NewSetup => solidcraft_engine::cmd::cam::known_machines(&app.session).into_iter().map(|e| (e.id, e.machine)).collect(),
        _ => Vec::new(),
    };
    // Points the designer placed (construction points), for the setup's origin.
    let points: Vec<String> = match ed.target {
        Target::Setup(_) | Target::NewSetup => app
            .session
            .model
            .state()
            .construct
            .iter()
            .filter(|c| matches!(c.geom, solidcraft_engine::doc::construct::ConstructGeom::Point(_)))
            .map(|c| c.name.clone())
            .collect(),
        _ => Vec::new(),
    };
    let picked = match ed.target {
        Target::Operation(_) | Target::NewOperation(_) => selection_geometry(app, ed.draft["type"].as_str().unwrap_or("")),
        _ => None,
    };
    // A command's form: its fields, and the design's bodies to pick from.
    let ed_auto = ed.auto.clone();
    let all_bodies: Vec<String> = match ed.target {
        Target::Command(_) => app.session.model.state().bodies.iter().map(|b| b.name.clone()).collect(),
        _ => Vec::new(),
    };
    let (mut ok, mut apply_now, mut cancel, mut reload) = (false, false, false, false);
    let mut quick_act: Option<QuickAction> = None;
    let mut pick_act: Option<PickAction> = None;
    // The full form's Pick on the Model: back to clicking what the operation works on.
    let mut repick = false;
    let can_pick = matches!(ed.target, Target::Operation(_) | Target::NewOperation(_)) && !pick_choices(ed.draft["type"].as_str().unwrap_or("")).0.is_empty();
    // What the pointer is on in the model, for the click panel's settings.
    let hover_shape = if ed.pick.is_some() { crate::toolbar::hover_shape(app) } else { None };
    // The form scrolls within most of the window's height.
    let tall = (ctx.content_rect().height() * 0.72).max(320.0);
    // Held by its top edge: opening and folding sections grows and shrinks it downwards
    // instead of moving it under the pointer.
    let area = ctx.content_rect();
    // Esc stops picking an origin; it does not close the editor (its close and Cancel do).
    let can = ed.blocked().is_none();
    // The click panel holds a few lines: narrower than the full form. Either can be dragged wider.
    let (closed, action) = crate::ui_kit::Window::new(egui::Id::unique("sc_cam_editor"), "ws_manufacture", &name)
        .cards(if ed.pick.is_some() { 1.6 } else { 2.5 })
        .resizable()
        .placed(egui::Align2::CENTER_TOP, egui::pos2(area.center().x, area.top() + t.space_24 * 5.0))
        .fields()
        .keep_open_on_escape()
        .actions("OK", "Cancel", Some("Apply"))
        .show(ctx, can, |ui| {
            // A new operation: click on the model, the form behind Adjust.
            if ed.pick.is_some() {
                {
                    let CamEditor { draft, pick, tool_from, shop_pick, inputs, .. } = &mut ed;
                    let mut from = ToolFrom { from: tool_from, libraries: &libraries, pick: shop_pick };
                    if let Some(p) = pick.as_ref() {
                        pick_act = pick_panel(ui, p, draft, inputs, hover_shape, &tools, &mut from, picked.as_ref().map(|x| x.1.as_str()));
                    }
                }
                if let Some((m, error)) = &ed.message {
                    ui.label(RichText::new(m).color(if *error { t.error } else if m.starts_with(GOOD) { t.form_success } else { t.warning }));
                }
                return;
            }
            // The two-click setup: what to click next, and nothing else.
            if let Some(step) = ed.quick {
                quick_act = quick_panel(ui, &mut ed, step);
                if let Some((m, error)) = &ed.message {
                    ui.label(RichText::new(m).color(if *error { t.error } else if m.starts_with(GOOD) { t.form_success } else { t.warning }));
                }
                return;
            }
            // Minimize folds the form down to this button and OK / Cancel, out of the way while
            // zooming in on the model; Restore brings the fields back as they were.
            let fold = if ed.minimized { "▼  Restore" } else { "▲  Minimize" };
            let hint = if ed.minimized { "Show the form's fields again" } else { "Fold the form away to see more of the model" };
            if crate::ui_kit::button(ui, solidcraft_ui_text::tr_label(fold), crate::ui_kit::Button::Normal)
                .on_hover_text(solidcraft_ui_text::tr_label(hint))
                .clicked()
            {
                ed.minimized = !ed.minimized;
            }
            if ed.minimized {
                return;
            }
            if can_pick
                && crate::ui_kit::button(ui, solidcraft_ui_text::tr_label("Pick on the Model"), crate::ui_kit::Button::Normal)
                    .on_hover_text(solidcraft_ui_text::tr_label("Click what this operation works on (a floor, walls, edges, holes) instead of typing it"))
                    .clicked()
            {
                repick = true;
            }
            ui.add_space(t.space_8);
            // Fields only: the JSON tab (a typed call) is not shown; scripts take the call directly.
            if ed.tab != Tab::Fields {
                switch(&mut ed, Tab::Fields);
            }
            if changed_elsewhere {
                ui.horizontal(|ui| {
                    let text = format!("The {} changed elsewhere since you opened it.", ed.target.noun());
                    ui.label(RichText::new(text).color(t.warning));
                    if crate::ui_kit::button(ui, "Reload", crate::ui_kit::Button::Normal).clicked() {
                        reload = true;
                    }
                });
            }
            match ed.tab {
                Tab::Fields => {
                    egui::ScrollArea::vertical().max_height(tall).auto_shrink([false, true]).show(ui, |ui| {
                        let CamEditor { draft, inputs, target, place, origin, tool_from, shop_pick, .. } = &mut ed;
                        let mut from = ToolFrom { from: tool_from, libraries: &libraries, pick: shop_pick };
                        match target {
                            Target::Operation(_) => fields(ui, draft, inputs, &tools, &mut from, used.as_deref(), picked.as_ref()),
                            Target::NewOperation(setup) => {
                                // Which setup it goes into (the one picked in the Browser to start with).
                                let mut k = setups.iter().position(|x| x.0 == *setup).unwrap_or(0);
                                let names: Vec<&str> = setups.iter().map(|x| x.1.as_str()).collect();
                                crate::ui_kit::row(ui, "Setup", |ui| crate::ui_kit::dropdown(ui, "cam_ed_setup", &mut k, &names));
                                if let Some(x) = setups.get(k) {
                                    *setup = x.0;
                                }
                                fields(ui, draft, inputs, &tools, &mut from, None, picked.as_ref());
                            }
                            Target::Tool(_) | Target::NewTool => tool_fields(ui, draft, inputs, uses),
                            Target::Setup(id) => {
                                if let Some(extra) = setup_fields(ui, draft, inputs, *id, &setups, origin, &points, &machines) {
                                    *place = Some(extra);
                                    apply_now = true;
                                }
                            }
                            // Not created yet: it comes after every setup there is.
                            Target::NewSetup => {
                                if let Some(extra) = setup_fields(ui, draft, inputs, u64::MAX, &setups, origin, &points, &machines) {
                                    *place = Some(extra);
                                    apply_now = true;
                                }
                            }
                            Target::Run(_) => run_form(ui, draft, inputs, &setups, &run_fields),
                            Target::Command(_) => auto_fields(ui, draft, inputs, &ed_auto, &all_bodies),
                        }
                    });
                }
                Tab::Json => {
                    egui::ScrollArea::vertical().max_height(tall).auto_shrink([false, true]).show(ui, |ui| {
                        let r = ui.add(
                            egui::TextEdit::multiline(&mut ed.text)
                                .id(egui::Id::unique("sc_cam_editor_json"))
                                .code_editor()
                                .desired_rows(24)
                                .desired_width(f32::INFINITY),
                        );
                        if r.changed() {
                            ed.text_error = serde_json::from_str::<Value>(&ed.text).err().map(|e| e.to_string());
                        }
                    });
                    if let Some(e) = &ed.text_error {
                        ui.label(RichText::new(e).color(t.error));
                    }
                }
            }
            if let Some(why) = ed.blocked().filter(|_| ed.tab == Tab::Fields) {
                ui.label(RichText::new(why).color(t.error));
            }
            if let Some((m, error)) = &ed.message {
                ui.label(RichText::new(m).color(if *error { t.error } else if m.starts_with(GOOD) { t.form_success } else { t.warning }));
            }
        });
    match action {
        crate::ui_kit::Action::Ok if can => ok = true,
        crate::ui_kit::Action::Apply if can => apply_now = true,
        crate::ui_kit::Action::Cancel => cancel = true,
        _ => {}
    }
    let open = !closed;
    if reload {
        ed.base = now.clone();
        ed.set_draft(now);
        ed.message = None;
    }
    // The two-click setup's buttons and its top-face click.
    if let (Some(a), Target::Setup(id)) = (quick_act, ed.target) {
        match a {
            QuickAction::Margin(m) => {
                if let Err(e) = app.run("cam.setup.set", json!({"setup": id, "stock": margin_stock(m)})) {
                    ed.message = Some((e, true));
                }
            }
            QuickAction::KeepZ => {
                ed.quick = Some(Quick::Zero);
                ed.origin.picking = true;
                ed.origin.before = None;
                app.set_status("Click your zero: a corner, edge middle, face centre or hole of the model or the stock.".to_string(), false);
            }
            QuickAction::Advanced => {
                ed.quick = None;
                ed.origin.picking = false;
                ed.minimized = false;
            }
        }
    }
    if ed.quick == Some(Quick::Top)
        && let Target::Setup(id) = ed.target
    {
        let sel = app.session.selection.clone();
        if ed.quick_before.as_ref() != Some(&sel) {
            match sel.last() {
                Some(face @ solidcraft_engine::Sel::Face { .. }) => {
                    let up = crate::dialogs_assembly::snap_of(&app.session, face).and_then(|s| wcs_up(app, id, s.z));
                    match up.map(|w| app.run("cam.setup.set", json!({"setup": id, "wcs": w}))) {
                        Some(Ok(_)) => {
                            ed.quick = Some(Quick::Zero);
                            ed.origin.picking = true;
                            ed.origin.before = None;
                            let _ = app.session.execute("select.clear", &json!({}));
                            app.set_status("Top set. Now click your zero: a corner, edge middle, face centre or hole of the model or the stock.".to_string(), false);
                        }
                        Some(Err(e)) => ed.message = Some((e, true)),
                        None => ed.message = Some(("That face has no direction to stand the part on: click a flat face.".to_string(), true)),
                    }
                    ed.quick_before = Some(app.session.selection.clone());
                }
                _ => ed.quick_before = Some(sel),
            }
        }
    }
    // Picking the origin on the model: the click that changes the selection places it, at the
    // snap of what it picked (a coordinate).
    // A click on a point of the stock (or away from the model) chooses the snap shown.
    if ed.origin.picking {
        let sel = &app.session.selection;
        let picked = match &ed.origin.before {
            _ if ed.origin.chosen.is_some() => ed.origin.chosen.take(),
            None => {
                ed.origin.before = Some(sel.clone());
                None
            }
            Some(before) if before != sel => {
                let at = ed.origin.current.or_else(|| sel.last().and_then(|x| crate::dialogs_assembly::snap_of(&app.session, x)).map(|s| s.at));
                if at.is_none() {
                    ed.origin.before = Some(sel.clone());
                }
                at
            }
            Some(_) => None,
        };
        if let Some(at) = picked {
            ed.place = Some(json!({"origin": [at.x, at.y, at.z]}));
            ed.origin.picking = false;
            ed.origin.current = None;
            apply_now = true;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ed.origin.picking = false;
        }
    }
    // A shop library tool picked in the Tool box: into the job, and into the operation.
    if let Some(index) = ed.shop_pick.take() {
        take_shop_tool(app, &mut ed, index);
    }
    // Pick on the Model from the full form: the click panel, picking what the kind picks first.
    if repick {
        let ty = ed.draft["type"].as_str().unwrap_or("").to_string();
        if let Some(first) = pick_choices(&ty).0.first() {
            ed.pick = Some(OpPick { what: *first, before: Some(app.session.selection.clone()), points: Vec::new(), sketch: None });
            app.set_status(solidcraft_ui_text::tr_label(pick_line(&ty, *first)).to_string(), false);
        }
    }
    // A new operation's panel buttons, then its clicks on the model.
    match pick_act {
        Some(PickAction::Switch(what)) => {
            if let Some(p) = ed.pick.as_mut() {
                p.what = what;
                p.points.clear();
                p.sketch = None;
                p.before = Some(app.session.selection.clone());
            }
            let ty = ed.draft["type"].as_str().unwrap_or("").to_string();
            app.set_status(solidcraft_ui_text::tr_label(pick_line(&ty, what)).to_string(), false);
        }
        Some(PickAction::Whole) => {
            if finish_pick(app, &mut ed, None) {
                return;
            }
        }
        Some(PickAction::Done) => {
            let g = ed.pick.as_ref().and_then(|p| pick_geometry(p.what, &p.points, p.sketch));
            if finish_pick(app, &mut ed, g) {
                return;
            }
        }
        Some(PickAction::UseSelected) => {
            if let Some((g, _)) = picked.clone()
                && finish_pick(app, &mut ed, Some(g))
            {
                return;
            }
        }
        Some(PickAction::Adjust) => ed.pick = None,
        None => {}
    }
    if let Some(p) = ed.pick.as_mut() {
        let sel = app.session.selection.clone();
        if p.before.as_ref() != Some(&sel) {
            let clicked = sel.last().and_then(|s| pick_of(s, p.what));
            p.before = Some(sel);
            if let Some((point, sketch)) = clicked {
                if let Some(point) = point
                    && !p.points.contains(&point)
                {
                    p.points.push(point);
                }
                if sketch.is_some() {
                    p.sketch = sketch;
                }
                // One click is enough, but rails take two.
                let ready = if p.what == PickWhat::Rails { p.points.len() >= 2 } else { !pick_many(p.what) };
                if ready {
                    let g = pick_geometry(p.what, &p.points, p.sketch);
                    if finish_pick(app, &mut ed, g) {
                        return;
                    }
                    // Refused: the next click tries again.
                    if let Some(p) = ed.pick.as_mut() {
                        p.points.clear();
                        p.sketch = None;
                    }
                }
            }
        }
    }
    if ok || apply_now {
        // The two-click setup's second click: once the zero is placed, it is done.
        let zero_placed = ed.quick == Some(Quick::Zero) && ed.place.is_some();
        let taken = apply(app, &mut ed);
        if (ok || zero_placed) && taken {
            if zero_placed {
                app.set_status("Setup ready: top and zero set. Add a tool and operations next.".to_string(), false);
            }
            return;
        }
    }
    if open && !cancel {
        app.cam_editor = Some(ed);
    }
}

/// Moves the edits to the other tab. A JSON text that does not parse keeps the JSON tab.
fn switch(ed: &mut CamEditor, to: Tab) {
    match to {
        Tab::Json => {
            ed.text = pretty(&ed.draft);
            ed.text_error = None;
            ed.tab = Tab::Json;
        }
        Tab::Fields => match serde_json::from_str::<Value>(&ed.text) {
            Ok(v) if v.is_object() => {
                let text = std::mem::take(&mut ed.text);
                ed.set_draft(v);
                ed.text = text;
                ed.tab = Tab::Fields;
            }
            Ok(_) => ed.text_error = Some(format!("the {} must be a JSON object", ed.target.noun())),
            Err(e) => ed.text_error = Some(e.to_string()),
        },
    }
}

/// The Fields tab.
/// `picked`: the geometry the viewport's selection would give, for Use Selected.
fn fields(
    ui: &mut egui::Ui,
    draft: &mut Value,
    inputs: &mut Inputs,
    tools: &[(u64, String)],
    from: &mut ToolFrom,
    used: Option<&str>,
    picked: Option<&(Value, String)>,
) {
    let Value::Object(op) = draft else { return };
    // Name, type, tool, enabled.
    if let Some(Value::String(name)) = op.get_mut("name") {
        crate::ui_kit::row(ui, "Name", |ui| crate::ui_kit::text_field(ui, name, ""));
    }
    let ty = op.get("type").and_then(Value::as_str).unwrap_or("").to_string();
    let kinds = cam::ops::OpKind::NAMES;
    let mut k = kinds.iter().position(|n| *n == ty).unwrap_or(0);
    // Preferences ▸ Names: the shop's or Fusion's words for each kind.
    let labels: Vec<String> = kinds
        .iter()
        .map(|n| solidcraft_ui_text::names::command(&format!("cam.operation.{n}")).map_or_else(|| kind_label(n), str::to_string))
        .collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    crate::ui_kit::row(ui, "Type", |ui| crate::ui_kit::dropdown(ui, "cam_ed_type", &mut k, &refs));
    if let Some(new) = kinds.get(k).filter(|n| **n != ty) {
        change_kind(op, new);
        inputs.text.clear();
        inputs.invalid.clear();
    }
    tool_rows(ui, op, tools, from);
    if let Some(Value::Bool(on)) = op.get_mut("enabled") {
        crate::ui_kit::checkbox(ui, "Enabled", on);
    }
    rest_of_fields(ui, op, inputs, used, picked);
}

/// The operation's Tool from and Tool rows: the job's own tools, or a shop library (a pick
/// copies the tool into the job, the library stays as it is).
fn tool_rows(ui: &mut egui::Ui, op: &mut Map<String, Value>, tools: &[(u64, String)], from: &mut ToolFrom) {
    if !from.libraries.is_empty() {
        let names: Vec<String> = std::iter::once("Job tools".to_string())
            .chain(from.libraries.iter().map(|(name, list)| format!("{name} ({})", list.len())))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        *from.from = (*from.from).min(refs.len() - 1);
        crate::ui_kit::row(ui, "Tool from", |ui| crate::ui_kit::dropdown(ui, "cam_ed_tool_from", from.from, &refs));
    }
    let tool = op.get("tool").and_then(Value::as_u64).unwrap_or(0);
    match from.libraries.get(from.from.wrapping_sub(1)).filter(|_| *from.from > 0) {
        Some((_, list)) => {
            let mut ti = usize::MAX;
            let labels: Vec<&str> = list.iter().map(|x| x.1.as_str()).collect();
            crate::ui_kit::row(ui, "Tool", |ui| crate::ui_kit::dropdown(ui, "cam_ed_tool_lib", &mut ti, &labels));
            if let Some((index, _)) = list.get(ti) {
                *from.pick = Some(*index);
            }
            let t = Tokens::get();
            ui.label(RichText::new("Picking a tool copies it into this job. Edit it there: the library stays as it is.").color(t.text_dim));
            ui.label(RichText::new(crate::shop_library::PLACEHOLDER_NOTE).color(t.text_dim));
        }
        None => {
            let mut ti = tools.iter().position(|x| x.0 == tool).unwrap_or(usize::MAX);
            let tool_labels: Vec<&str> = tools.iter().map(|x| x.1.as_str()).collect();
            crate::ui_kit::row(ui, "Tool", |ui| crate::ui_kit::dropdown(ui, "cam_ed_tool", &mut ti, &tool_labels));
            if let Some((id, _)) = tools.get(ti).filter(|x| x.0 != tool) {
                op.insert("tool".into(), json!(id));
            }
        }
    }
}

/// The operation form below its tool: what it works on, its parameters and the folded sections.
fn rest_of_fields(ui: &mut egui::Ui, op: &mut Map<String, Value>, inputs: &mut Inputs, used: Option<&str>, picked: Option<&(Value, String)>) {
    let ty = op.get("type").and_then(Value::as_str).unwrap_or("").to_string();
    // What the operation works on, and a way to take it from the viewport's selection, at the
    // top where it is seen first.
    let bounded = serde_json::from_value::<cam::ops::OpKind>(json!({"type": ty})).is_ok_and(|k| k.uses_boundary());
    let picks = op.get("geometry").and_then(geometry_picks);
    let wants_selection = op.get("boundary").and_then(|b| b.get("source")).and_then(Value::as_str) == Some("selection");
    let t = Tokens::get();
    match &picks {
        Some(w) => {
            ui.label(RichText::new(format!("Selection: {w}")).color(t.text_dim));
        }
        None if wants_selection => {
            ui.label(RichText::new("Nothing is picked: select faces or a sketch in the viewport, then Use Selected").color(t.warning));
        }
        None => {}
    }
    if let Some((g, what)) = picked
        && crate::ui_kit::button(ui, format!("Use Selected ({what})"), crate::ui_kit::Button::Normal)
            .on_hover_text("Work on what is selected in the viewport: sets the geometry (and an area operation's boundary) to the selection")
            .clicked()
    {
        op.insert("geometry".into(), g.clone());
        inputs.text.retain(|k, _| !k.starts_with("geometry."));
        inputs.invalid.retain(|k| !k.starts_with("geometry."));
        if bounded {
            let b = op.entry("boundary").or_insert_with(|| serde_json::to_value(cam::boundary::Boundary::default()).unwrap_or(Value::Null));
            if let Value::Object(b) = b {
                b.insert("source".into(), json!("selection"));
            }
            inputs.text.remove("boundary.source");
        }
    }
    // The kind's own parameters.
    let choices = cam::ops::OpKind::choices(&ty);
    let mut keys: Vec<String> = cam::ops::OpKind::defaults(&ty).and_then(|d| d.as_object().map(|o| o.keys().cloned().collect())).unwrap_or_default();
    keys.extend(op.keys().filter(|k| !COMMON.contains(&k.as_str()) && !keys.contains(k)).cloned().collect::<Vec<_>>());
    keys.retain(|k| k != "type" && op.contains_key(k));
    section(ui, "Parameters", true, |ui| {
        for key in &keys {
            let list = choices.iter().find(|(k, _)| k == key).map(|x| x.1.as_slice());
            if let Some(v) = op.get_mut(key) {
                value(ui, inputs, key, &label_of(key, v), v, list);
            }
        }
    });
    section(ui, "Stock to leave", false, |ui| {
        for (key, label) in [
            ("stock_to_leave_radial", "Radial (mm)"),
            ("stock_to_leave_axial", "Axial (mm)"),
            ("rest_machining", "Rest machining"),
            ("tolerance", "Tolerance (mm)"),
        ] {
            if let Some(v) = op.get_mut(key) {
                value(ui, inputs, key, solidcraft_ui_text::names::cam(label), v, None);
            }
        }
    });
    section(ui, "Orientation", false, |ui| {
        let mut tilted = op.contains_key("orientation");
        crate::ui_kit::checkbox(ui, "Tilted tool axis (3+2)", &mut tilted).on_hover_text("orientation");
        if tilted && !op.contains_key("orientation") {
            op.insert("orientation".into(), json!({"origin": [0.0, 0.0, 0.0], "x": [1.0, 0.0, 0.0], "y": [0.0, 1.0, 0.0], "z": [0.0, 0.0, 1.0]}));
        } else if !tilted {
            op.remove("orientation");
            inputs.text.retain(|k, _| !k.starts_with("orientation."));
            inputs.invalid.retain(|k| !k.starts_with("orientation."));
        }
        if let Some(Value::Object(o)) = op.get_mut("orientation") {
            for (key, label) in [("origin", "Origin"), ("x", "X axis"), ("y", "Y axis"), ("z", "Tool axis (Z)")] {
                if let Some(v) = o.get_mut(key) {
                    value(ui, inputs, &format!("orientation.{key}"), label, v, None);
                }
            }
        }
    });
    // Where the operation may machine (area operations).
    if bounded {
        if !op.contains_key("boundary") {
            op.insert("boundary".into(), serde_json::to_value(cam::boundary::Boundary::default()).unwrap_or(Value::Null));
        }
        if let Some(Value::Object(b)) = op.get_mut("boundary") {
            section(ui, "Boundary", true, |ui| {
                let t = Tokens::get();
                let sources = cam::ops::variants::<cam::boundary::Source>(Value::Null);
                let contain = cam::ops::variants::<cam::boundary::Containment>(Value::Null);
                for (key, label) in [
                    ("source", "Machine within"),
                    ("containment", "Tool"),
                    ("offset", "Additional offset (mm)"),
                    ("clip_to_stock", "Stay within the stock"),
                    ("avoid_fixtures", "Keep clear of fixtures"),
                    ("fixture_clearance", "Fixture clearance (mm)"),
                ] {
                    let choices = match key {
                        "source" => Some(sources.as_slice()),
                        "containment" => Some(contain.as_slice()),
                        _ => None,
                    };
                    if let Some(v) = b.get_mut(key) {
                        value(ui, inputs, &format!("boundary.{key}"), label, v, choices);
                    }
                }
                let hint = "Auto: roughing clears around the part's outline, wall finishing reaches the walls from outside, surface finishing keeps to the part, and a selection bounds the operation to what was picked.";
                ui.label(RichText::new(solidcraft_ui_text::tr_label(hint)).color(t.text_dim));
                if let Some(u) = used {
                    ui.label(RichText::new(format!("Last generated: {u}")).color(t.text_dim));
                }
            });
        }
    }
    if let Some(g) = op.get_mut("geometry") {
        section(ui, "Geometry", false, |ui| {
            let sources = cam::ops::variants::<cam::setup::Geometry>(json!({"source": "\u{1}"}));
            tagged(ui, inputs, "geometry", g, "source", &sources);
        });
    }
    if let Some(Value::Object(h)) = op.get_mut("heights") {
        section(ui, solidcraft_ui_text::names::cam("Heights"), false, |ui| {
            let from = cam::ops::variants::<cam::linking::HeightRef>(Value::Null);
            for (key, label) in HEIGHTS {
                if let Some(v) = h.get_mut(key) {
                    height(ui, inputs, &format!("heights.{key}"), solidcraft_ui_text::names::cam(label), v, &from);
                }
            }
        });
    }
    if let Some(Value::Object(l)) = op.get_mut("linking") {
        section(ui, solidcraft_ui_text::names::cam("Linking"), false, |ui| {
            let keys: Vec<String> = l.keys().cloned().collect();
            for key in keys {
                let path = format!("linking.{key}");
                let Some(v) = l.get_mut(&key) else { continue };
                if key == "ramp" {
                    let ramps = cam::ops::variants::<cam::linking::Ramp>(json!({"type": "\u{1}"}));
                    group(ui, "Ramp", |ui| tagged(ui, inputs, &path, v, "type", &ramps));
                } else {
                    let label = label_of(&key, v);
                    value(ui, inputs, &path, &label, v, None);
                }
            }
        });
    }
}

/// The operation form's Tool box source: which list it shows, the shop libraries (each with its
/// tools' library indices and labels), and the library tool picked this frame.
struct ToolFrom<'a> {
    from: &'a mut usize,
    libraries: &'a [(String, Vec<(usize, String)>)],
    pick: &'a mut Option<usize>,
}

/// The shop library's tools by library, in the order the libraries come.
fn shop_libraries(list: Option<&Value>) -> Vec<(String, Vec<(usize, String)>)> {
    let mut out: Vec<(String, Vec<(usize, String)>)> = Vec::new();
    let Some(tools) = list.and_then(|l| l["tools"].as_array()) else { return out };
    // By kind, then smallest first: the kind leads each label (a dropdown has no headings).
    let mut order: Vec<usize> = (0..tools.len()).collect();
    let key = |i: &usize| {
        let tl = &tools[*i]["tool"];
        (crate::shop_library::kind_rank(tl["kind"].as_str().unwrap_or("")), tl["diameter"].as_f64().unwrap_or(0.0))
    };
    order.sort_by(|a, b| key(a).0.cmp(&key(b).0).then(key(a).1.total_cmp(&key(b).1)));
    for i in order {
        let e = &tools[i];
        let index = e["index"].as_u64().map_or(i, |n| n as usize);
        let tl = &e["tool"];
        let number = tl["number"].as_u64().unwrap_or(0);
        let label = format!(
            "{}  ·  {} mm  ·  {}{}",
            crate::shop_library::kind_word(tl["kind"].as_str().unwrap_or("")),
            (tl["diameter"].as_f64().unwrap_or(0.0) * 1000.0).round() / 1000.0,
            if number > 0 { format!("T{number} ") } else { String::new() },
            tl["name"].as_str().unwrap_or(""),
        );
        let library = e["library"].as_str().unwrap_or("").to_string();
        match out.iter_mut().find(|(n, _)| *n == library) {
            Some((_, list)) => list.push((index, label)),
            None => out.push((library, vec![(index, label)])),
        }
    }
    out
}

/// Puts the shop library tool `index` into the operation being edited: the job's own copy when
/// it has one already (same name, kind and diameter), else a new copy.
fn take_shop_tool(app: &mut SolveApp, ed: &mut CamEditor, index: usize) {
    let entry = ed.shop.as_ref().and_then(|l| l["tools"].as_array()).and_then(|t| t.iter().find(|e| e["index"].as_u64() == Some(index as u64)));
    let Some(entry) = entry.cloned() else { return };
    let want = &entry["tool"];
    let same = app.session.doc.manufacture.tools.tools.iter().find(|t| {
        let v = serde_json::to_value(t).unwrap_or(Value::Null);
        v["name"] == want["name"] && v["kind"] == want["kind"] && (t.diameter - want["diameter"].as_f64().unwrap_or(-1.0)).abs() < 1e-6
    });
    let library = entry["library"].as_str().unwrap_or("").to_string();
    let (id, message) = match same {
        Some(t) => (t.id, format!("{GOOD}T{} {} is already in this job: picked it.", t.number, t.name)),
        None => match app.run("cam.library.add", json!({"indices": [index]})) {
            Ok(v) => {
                let t = &v["tools"][0];
                let Some(id) = t["id"].as_u64() else { return };
                (id, format!("{GOOD}T{} {} copied into this job from {library}.", t["number"], t["name"].as_str().unwrap_or("")))
            }
            Err(e) => {
                ed.message = Some((e, true));
                return;
            }
        },
    };
    if let Value::Object(op) = &mut ed.draft {
        op.insert("tool".into(), json!(id));
    }
    ed.tool_from = 0;
    ed.message = Some((message, false));
}

/// The Fields tab of a tool.
fn tool_fields(ui: &mut egui::Ui, draft: &mut Value, inputs: &mut Inputs, uses: usize) {
    let Value::Object(tl) = draft else { return };
    let t = Tokens::get();
    if uses > 0 {
        let s = if uses == 1 { "" } else { "s" };
        let text = format!("Used by {uses} operation{s}: a change makes their toolpaths stale.");
        ui.label(RichText::new(text).color(t.text_dim));
    }
    if let Some(Value::String(name)) = tl.get_mut("name") {
        crate::ui_kit::row(ui, "Name", |ui| crate::ui_kit::text_field(ui, name, ""));
    }
    for (key, label) in [("number", "Tool number (T)"), ("length_offset", "Length offset (H)")] {
        if let Some(v) = tl.get_mut(key) {
            value(ui, inputs, key, label, v, None);
        }
    }
    // The kind, then its own sizes.
    let kinds = cam::ops::variants::<cam::tool::ToolKind>(json!({"kind": "\u{1}"}));
    let current = tl.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
    let mut k = kinds.iter().position(|c| *c == current).unwrap_or(0);
    let labels: Vec<String> = kinds.iter().map(|c| kind_label(c)).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    crate::ui_kit::row(ui, "Kind", |ui| crate::ui_kit::dropdown(ui, "cam_ed_tool_kind", &mut k, &refs));
    if let Some(new) = kinds.get(k).filter(|c| **c != current) {
        let diameter = tl.get("diameter").and_then(Value::as_f64).unwrap_or(3.0);
        tl.retain(|key, _| !TOOL_KIND_KEYS.contains(&key.as_str()));
        tl.insert("kind".into(), json!(new));
        if let Value::Object(d) = tool_kind_default(new, diameter) {
            for key in d.keys() {
                inputs.text.remove(key);
                inputs.invalid.remove(key);
            }
            tl.extend(d);
        }
        inputs.text.retain(|key, _| !TOOL_KIND_KEYS.contains(&key.as_str()));
        inputs.invalid.retain(|key| !TOOL_KIND_KEYS.contains(&key.as_str()));
    }
    for key in TOOL_KIND_KEYS {
        if let Some(v) = tl.get_mut(*key) {
            let label = label_of(key, v);
            value(ui, inputs, key, &label, v, None);
        }
    }
    let kind_now = tl.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
    section(ui, "Size", true, |ui| {
        // A torch or a beam has no body: only its width matters (the kerf, the spot).
        let beam = matches!(kind_now.as_str(), "plasma" | "laser");
        let diameter_label = match kind_now.as_str() {
            "plasma" => "Kerf width (mm)",
            "laser" => "Spot size (mm)",
            "boring_bar" => "Smallest bore (mm)",
            "tap" => "Thread diameter (mm)",
            _ => "Diameter (mm)",
        };
        let necked = matches!(kind_now.as_str(), "t_slot" | "slitting_saw" | "dovetail" | "lollipop");
        let (flute_label, shank_label) = match kind_now.as_str() {
            "t_slot" | "slitting_saw" => ("Disc thickness (mm)", "Neck diameter (mm)"),
            _ if necked => ("Cutting height (mm)", "Neck diameter (mm)"),
            _ => ("Flute length (mm)", "Shank diameter (mm)"),
        };
        for (key, label) in [
            ("diameter", diameter_label),
            ("flute_length", flute_label),
            ("shank_diameter", shank_label),
            ("overall_length", "Stick-out (mm)"),
            ("holder_diameter", "Holder diameter (mm)"),
            ("flutes", "Flutes"),
        ] {
            if beam && key != "diameter" {
                continue;
            }
            if let Some(v) = tl.get_mut(key) {
                value(ui, inputs, key, label, v, None);
            }
        }
    });
    let beam = matches!(kind_now.as_str(), "plasma" | "laser");
    if let Some(Value::Object(f)) = tl.get_mut("feeds")
        && !beam
    {
        section(ui, "Feeds and speeds", true, |ui| {
            let coolants = cam::ops::variants::<cam::tool::Coolant>(Value::Null);
            let keys: Vec<String> = f.keys().cloned().collect();
            for key in keys {
                let path = format!("feeds.{key}");
                let Some(v) = f.get_mut(&key) else { continue };
                let label = match key.as_str() {
                    "spindle_rpm" => "Spindle speed (rev/min)".to_string(),
                    "clockwise" => "Clockwise (M3)".to_string(),
                    "coolant" => "Coolant".to_string(),
                    _ if v.is_number() => format!("{} feed (mm/min)", kind_label(&key)),
                    _ => kind_label(&key),
                };
                let choices = (key == "coolant").then_some(coolants.as_slice());
                value(ui, inputs, &path, &label, v, choices);
            }
        });
    }
    // Anything newer than this form.
    let known = [
        "name",
        "number",
        "length_offset",
        "kind",
        "diameter",
        "flute_length",
        "shank_diameter",
        "overall_length",
        "holder_diameter",
        "flutes",
        "feeds",
    ];
    let rest: Vec<String> = tl.keys().filter(|k| !known.contains(&k.as_str()) && !TOOL_KIND_KEYS.contains(&k.as_str())).cloned().collect();
    if !rest.is_empty() {
        section(ui, "Other", false, |ui| {
            for key in rest {
                if let Some(v) = tl.get_mut(&key) {
                    let label = label_of(&key, v);
                    value(ui, inputs, &key, &label, v, None);
                }
            }
        });
    }
}

/// The Fields tab of a setup; returns an axis when a Turn Over button was pressed.
/// The setup's form; returns what to add to `cam.setup.set` when a button applies at once
/// (turn over, place the origin).
fn setup_fields(
    ui: &mut egui::Ui,
    draft: &mut Value,
    inputs: &mut Inputs,
    id: u64,
    setups: &[(u64, String)],
    of: &mut OriginForm,
    points: &[String],
    machines: &[(String, cam::machine::Machine)],
) -> Option<Value> {
    let Value::Object(st) = draft else { return None };
    let t = Tokens::get();
    let mut flip: Option<Value> = None;
    if let Some(Value::String(name)) = st.get_mut("name") {
        crate::ui_kit::row(ui, "Name", |ui| crate::ui_kit::text_field(ui, name, ""));
    }
    if let Some(Value::String(program)) = st.get_mut("program") {
        crate::ui_kit::row(ui, "Program", |ui| crate::ui_kit::text_field(ui, program, "the setup's name"));
    }
    // The stock, and where it comes from.
    // A setup not created yet comes after every one there is.
    let here = setups.iter().position(|x| x.0 == id).unwrap_or(setups.len());
    if let Some(stock) = st.get_mut("stock") {
        section(ui, "Stock", true, |ui| {
            let modes = cam::ops::variants::<cam::setup::Stock>(json!({"mode": "\u{1}"}));
            let axes = cam::ops::variants::<cam::setup::Axis>(Value::Null);
            tagged_with(ui, inputs, "stock", stock, "mode", &modes, &[("axis", axes)]);
            if stock["mode"] == "from_setup" {
                let from = stock["setup"].as_u64().unwrap_or(0);
                let source = if from == 0 { here.checked_sub(1).and_then(|i| setups.get(i)) } else { setups.iter().take(here).find(|x| x.0 == from) };
                let text = match source {
                    Some((_, n)) => format!("Starts from what `{n}` leaves after its operations (setup 0: the one before)."),
                    None => "There is no earlier setup to take the stock from.".to_string(),
                };
                ui.label(RichText::new(text).color(if source.is_some() { t.text_dim } else { t.error }));
            }
        });
    }
    // The origin and axes every operation of the setup is measured from.
    if let Some(Value::Object(w)) = st.get_mut("wcs") {
        section(ui, "Origin and axes", true, |ui| {
            for (key, label) in [("origin", "Origin"), ("x", "X axis"), ("y", "Y axis"), ("z", "Tool axis (Z)")] {
                if let Some(v) = w.get_mut(key) {
                    value(ui, inputs, &format!("wcs.{key}"), label, v, None);
                }
            }
            ui.label(
                RichText::new("Turn the part over to machine its other side; the origin moves to the stock's front-left top corner.")
                    .color(t.text_dim),
            );
            ui.horizontal(|ui| {
                if crate::ui_kit::button(ui, "Turn Over About X", crate::ui_kit::Button::Normal).clicked() {
                    flip = Some(json!({"flip": "x"}));
                }
                if crate::ui_kit::button(ui, "Turn Over About Y", crate::ui_kit::Button::Normal).clicked() {
                    flip = Some(json!({"flip": "y"}));
                }
            });
            // Put the origin somewhere: a point of the stock or the model, a point the designer
            // placed, or picked on the model (it snaps as the pointer moves).
            if let Some(extra) = origin_rows(ui, of, points) {
                flip = Some(extra);
            }
        });
    }
    // The machine: a preset, then its properties.
    if let Some(machine) = st.get_mut("machine") {
        section(ui, "Machine", false, |ui| {
            // Every known machine: the machine files SolidCraft ships and the design's own.
            let mut labels: Vec<String> = vec![solidcraft_ui_text::tr_label("Keep this machine").to_string()];
            labels.extend(machines.iter().map(|(_, m)| m.name.clone()));
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let mut k = 0usize;
            crate::ui_kit::row(ui, "Preset", |ui| crate::ui_kit::dropdown(ui, "cam_ed_machine_preset", &mut k, &refs));
            if let Some(m) = k.checked_sub(1).and_then(|i| machines.get(i)).map(|x| x.1.clone()) {
                if let Ok(v) = serde_json::to_value(m) {
                    *machine = v;
                }
                inputs.text.retain(|key, _| !key.starts_with("machine."));
                inputs.invalid.retain(|key| !key.starts_with("machine."));
            }
            let Value::Object(mo) = machine else { return };
            let changers = cam::ops::variants::<cam::machine::ToolChanger>(json!({"kind": "\u{1}"}));
            let keys: Vec<String> = mo.keys().cloned().collect();
            for key in keys {
                let path = format!("machine.{key}");
                let Some(v) = mo.get_mut(&key) else { continue };
                match key.as_str() {
                    "tool_changer" => group(ui, "Tool changer", |ui| tagged(ui, inputs, &path, v, "kind", &changers)),
                    "laser" => {
                        let mut on = !v.is_null();
                        crate::ui_kit::checkbox(ui, "Laser module", &mut on).on_hover_text(path.as_str());
                        if on && v.is_null() {
                            *v = json!({"power_w": 2.5, "wavelength_nm": 445.0, "builtin": false, "spot_mm": 0.1});
                        } else if !on {
                            *v = Value::Null;
                        }
                        if v.is_object() {
                            value(ui, inputs, &path, "Laser", v, None);
                        }
                    }
                    _ => {
                        let label = match key.as_str() {
                            "spindle_rpm_min" => "Spindle min (rev/min)".to_string(),
                            "spindle_rpm_max" => "Spindle max (rev/min)".to_string(),
                            "spindle_power_w" => "Spindle power (W)".to_string(),
                            "max_feed" | "rapid_feed" => format!("{} (mm/min)", kind_label(&key)),
                            "rotary_rapid_deg_min" => "Rotary rapid (°/min)".to_string(),
                            "work_area" => "Work area (mm)".to_string(),
                            "collets" => "Collets (mm)".to_string(),
                            "rotary_capacity" => "Rotary capacity (mm)".to_string(),
                            _ => label_of(&key, v),
                        };
                        value(ui, inputs, &path, &label, v, None);
                    }
                }
            }
        });
    }
    // What is machined and what is avoided.
    section(ui, "Bodies", false, |ui| {
        for (key, label) in [("bodies", "Bodies to machine (none: all)"), ("fixtures", "Fixtures to avoid")] {
            if let Some(v) = st.get_mut(key) {
                value(ui, inputs, key, label, v, None);
            }
        }
    });
    flip
}

/// A plain command's form: the setup it is for (when it takes one), then a dropdown for each
/// parameter with choices, then any other parameter as its own field.
fn run_form(ui: &mut egui::Ui, draft: &mut Value, inputs: &mut Inputs, setups: &[(u64, String)], choices: &[(&'static str, &'static str, Vec<String>)]) {
    let Value::Object(p) = draft else { return };
    if let Some(current) = p.get("setup").and_then(Value::as_u64) {
        let mut k = setups.iter().position(|x| x.0 == current).unwrap_or(0);
        let names: Vec<&str> = setups.iter().map(|x| x.1.as_str()).collect();
        crate::ui_kit::row(ui, "Setup", |ui| crate::ui_kit::dropdown(ui, "cam_ed_run_setup", &mut k, &names));
        if let Some(x) = setups.get(k) {
            p.insert("setup".into(), json!(x.0));
        }
    }
    for (key, label, list) in choices {
        let v = p.entry(key.to_string()).or_insert_with(|| json!(list.first().cloned().unwrap_or_default()));
        value(ui, inputs, key, label, v, Some(list.as_slice()));
    }
    let rest: Vec<String> = p.keys().filter(|k| k.as_str() != "setup" && !choices.iter().any(|c| c.0 == k.as_str())).cloned().collect();
    for key in rest {
        if let Some(v) = p.get_mut(&key) {
            let label = label_of(&key, v);
            value(ui, inputs, &key, &label, v, None);
        }
    }
}

/// A command's form ([`Target::Command`]): one row per parameter, as [`crate::auto_form`] reads
/// them. A parameter left empty is left out, so the command uses its default.
fn auto_fields(ui: &mut egui::Ui, draft: &mut Value, inputs: &mut Inputs, fields: &[crate::auto_form::AutoField], bodies: &[String]) {
    let t = Tokens::get();
    let Value::Object(p) = draft else { return };
    if fields.is_empty() {
        ui.label(RichText::new(solidcraft_ui_text::tr_label("This command has nothing to set: press OK to run it.")).color(t.text_dim));
    }
    // What has to be answered, then everything else folded away under Advanced.
    for f in fields.iter().filter(|f| f.required) {
        auto_field(ui, p, inputs, f, bodies);
    }
    let optional: Vec<&crate::auto_form::AutoField> = fields.iter().filter(|f| !f.required).collect();
    if !optional.is_empty() {
        if fields.iter().all(|f| !f.required) {
            ui.label(RichText::new(solidcraft_ui_text::tr_label("Nothing has to be set: press OK, or open Advanced.")).color(t.text_dim));
        }
        section(ui, "Advanced", false, |ui| {
            for f in optional {
                auto_field(ui, p, inputs, f, bodies);
            }
        });
    }
}

/// One row of a command's form.
fn auto_field(ui: &mut egui::Ui, p: &mut Map<String, Value>, inputs: &mut Inputs, f: &crate::auto_form::AutoField, bodies: &[String]) {
    use crate::auto_form::AutoKind;
    let t = Tokens::get();
    {
        let label = if f.required { format!("{} *", f.label) } else { f.label.clone() };
        let key = f.key.as_str();
        let hint = if f.required { "" } else { "default" };
        match &f.kind {
            AutoKind::Bool => {
                let mut b = p.get(key).and_then(Value::as_bool).unwrap_or(false);
                if crate::ui_kit::checkbox(ui, &label, &mut b).on_hover_text(&f.help).changed() {
                    p.insert(key.into(), json!(b));
                }
            }
            AutoKind::Integer | AutoKind::Number | AutoKind::Length => {
                let shown = match p.get(key) {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) if v.is_number() => number_text(v),
                    _ => String::new(),
                };
                let integer = f.kind == AutoKind::Integer;
                let length = f.kind == AutoKind::Length;
                let got = typed(ui, inputs, key, &label, &shown, hint, |s| {
                    let s = s.trim();
                    if s.is_empty() {
                        return Some(Value::Null);
                    }
                    parse_number(s, integer).or_else(|| length.then(|| json!(s)))
                });
                match got {
                    Some(Value::Null) => {
                        p.remove(key);
                    }
                    Some(v) => {
                        p.insert(key.into(), v);
                    }
                    None => {}
                }
            }
            AutoKind::Text => {
                let shown = p.get(key).and_then(Value::as_str).unwrap_or("").to_string();
                match typed(ui, inputs, key, &label, &shown, hint, |s| Some(if s.trim().is_empty() { Value::Null } else { json!(s) })) {
                    Some(Value::Null) => {
                        p.remove(key);
                    }
                    Some(v) => {
                        p.insert(key.into(), v);
                    }
                    None => {}
                }
            }
            AutoKind::Point => {
                let shown = p.get(key).and_then(Value::as_array).map(|a| point_text(a)).unwrap_or_default();
                match typed(ui, inputs, key, &label, &shown, if f.required { "x, y, z" } else { "default" }, parse_point) {
                    Some(Value::Null) => {
                        p.remove(key);
                    }
                    Some(v) => {
                        p.insert(key.into(), v);
                    }
                    None => {}
                }
            }
            AutoKind::Choice(list) => {
                let _ = list_choice(ui, p, key, &label, list, f.required, false, &f.help);
            }
            AutoKind::Body if !bodies.is_empty() => {
                let _ = list_choice(ui, p, key, &label, bodies, f.required, true, &f.help);
            }
            AutoKind::Body => {
                ui.label(RichText::new(format!("{label}: there are no bodies to pick.")).color(t.error));
            }
            AutoKind::Bodies => {
                let mut picked: Vec<String> =
                    p.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
                let mut changed = false;
                group(ui, &format!("{label} ({})", picked.len()), |ui| {
                    for b in bodies {
                        let mut on = picked.contains(b);
                        if crate::ui_kit::checkbox(ui, b, &mut on).changed() {
                            changed = true;
                            if on {
                                picked.push(b.clone());
                            } else {
                                picked.retain(|x| x != b);
                            }
                        }
                    }
                });
                if changed {
                    if picked.is_empty() {
                        p.remove(key);
                    } else {
                        p.insert(key.into(), json!(picked));
                    }
                }
            }
            AutoKind::Other => {
                ui.label(RichText::new(format!("{label}: set by picking in the model or the command's own tool.")).color(t.text_dim));
            }
        }
    }
}

/// A dropdown of `list` (bodies or words); an optional one starts at "(default)", which leaves
/// the parameter out.
#[allow(clippy::too_many_arguments)]
fn list_choice(ui: &mut egui::Ui, p: &mut Map<String, Value>, key: &str, label: &str, list: &[String], required: bool, names: bool, help: &str) -> bool {
    let mut opts: Vec<String> = Vec::with_capacity(list.len() + 1);
    if !required {
        opts.push("(default)".into());
    }
    opts.extend(list.iter().cloned());
    let current = p.get(key).and_then(Value::as_str);
    let mut i = current.and_then(|c| opts.iter().position(|o| o == c)).unwrap_or(0);
    let before = i;
    let labels: Vec<String> = opts.iter().map(|o| if names || o.starts_with('(') { o.clone() } else { kind_label(o) }).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    crate::ui_kit::row(ui, label, |ui| crate::ui_kit::dropdown(ui, &format!("auto_form:{key}"), &mut i, &refs)).on_hover_text(help);
    if !required && i == 0 {
        p.remove(key);
    } else if let Some(o) = opts.get(i) {
        p.insert(key.into(), json!(o));
    }
    i != before
}

/// The sizes a tool kind starts with (a tool of `diameter` mm).
fn tool_kind_default(kind: &str, diameter: f64) -> Value {
    match kind {
        "bull_nose" => json!({"corner_radius": (diameter * 0.1).max(0.1)}),
        "chamfer" | "spot_drill" => json!({"tip_angle_deg": 90.0}),
        "drill" => json!({"tip_angle_deg": 118.0}),
        "laser" => json!({"wavelength_nm": 445.0}),
        "plasma" => json!({"pierce_height": 3.8, "cut_height": 1.5, "pierce_delay_s": 0.5}),
        "tapered_ball" => json!({"tip_radius": (diameter * 0.25).max(0.1), "taper_angle_deg": 5.0}),
        // Cutters on a neck start with one: thinner than the cutting part.
        "t_slot" => json!({"shank_diameter": diameter * 0.4, "flute_length": (diameter * 0.35).max(0.5)}),
        "slitting_saw" => json!({"shank_diameter": diameter * 0.3, "flute_length": (diameter * 0.05).max(0.5)}),
        "lollipop" => json!({"shank_diameter": diameter * 0.5, "flute_length": diameter}),
        "dovetail" => json!({"angle_deg": 60.0, "shank_diameter": diameter * 0.4, "flute_length": (diameter * 0.3).max(0.5)}),
        "tap" | "thread_mill" => json!({"pitch": 1.0}),
        "corner_round" => json!({"round_radius": (diameter * 0.25).max(0.1)}),
        "engraver" => json!({"tip_angle_deg": 30.0, "tip_diameter": (0.1f64).min(diameter * 0.25)}),
        _ => json!({}),
    }
}

/// A collapsible section of the form.
fn section(ui: &mut egui::Ui, title: &str, open: bool, add: impl FnOnce(&mut egui::Ui)) {
    let t = Tokens::get();
    egui::CollapsingHeader::new(RichText::new(solidcraft_ui_text::tr_label(title)).strong().color(t.text))
        .id_salt(("cam_ed_section", title))
        .default_open(open)
        .show(ui, add);
}

/// A nested group (a lead, the ramp) inside a section.
fn group(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(solidcraft_ui_text::tr_label(title)).id_salt(("cam_ed_group", title)).default_open(true).show(ui, add);
}

/// One parameter: a checkbox, a number, a choice, a text, a point, or a group of them.
fn value(ui: &mut egui::Ui, inputs: &mut Inputs, path: &str, label: &str, v: &mut Value, choices: Option<&[String]>) {
    match v {
        Value::Bool(b) => {
            crate::ui_kit::checkbox(ui, label, b).on_hover_text(path);
        }
        Value::Number(_) => {
            let integer = v.is_u64() || v.is_i64();
            let shown = number_text(v);
            if let Some(n) = typed(ui, inputs, path, label, &shown, "", |s| parse_number(s, integer)) {
                *v = n;
            }
        }
        Value::String(s) => match choices {
            Some(list) if !list.is_empty() => {
                let mut i = list.iter().position(|c| c == s).unwrap_or(0);
                let labels: Vec<String> = list.iter().map(|c| kind_label(c)).collect();
                let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                crate::ui_kit::row(ui, label, |ui| crate::ui_kit::dropdown(ui, &format!("cam_ed:{path}"), &mut i, &refs)).on_hover_text(path);
                if let Some(c) = list.get(i).filter(|c| *c != s) {
                    *s = c.clone();
                }
            }
            _ => {
                crate::ui_kit::row(ui, label, |ui| crate::ui_kit::text_field(ui, s, ""));
            }
        },
        // Not set: a point typed as "x, y" sets it, an empty field leaves it automatic.
        Value::Null => {
            if let Some(p) = typed(ui, inputs, path, label, "", "auto", parse_point) {
                *v = p;
            }
        }
        Value::Object(o) => {
            group(ui, label, |ui| {
                let keys: Vec<String> = o.keys().cloned().collect();
                for key in keys {
                    let p = format!("{path}.{key}");
                    if let Some(x) = o.get_mut(&key) {
                        let l = label_of(&key, x);
                        value(ui, inputs, &p, &l, x, None);
                    }
                }
            });
        }
        Value::Array(a) if is_point(a) => {
            let shown = point_text(a);
            let hint = if a.len() == 2 { "x, y" } else { "x, y, z" };
            if let Some(p) = typed(ui, inputs, path, label, &shown, hint, parse_point) {
                *v = p;
            }
        }
        Value::Array(a) => list(ui, inputs, path, label, a),
    }
}

/// The most rows a list shows; the rest are in the JSON tab.
const LIST_ROWS: usize = 500;

/// A list (picked points, a loop's points, the loops): a row per item with a remove button,
/// and Add, which repeats the last item (or starts a point) for editing.
fn list(ui: &mut egui::Ui, inputs: &mut Inputs, path: &str, label: &str, a: &mut Vec<Value>) {
    let t = Tokens::get();
    let mut remove = None;
    let mut add = false;
    let title = format!("{label} ({})", a.len());
    egui::CollapsingHeader::new(title).id_salt(("cam_ed_list", path)).default_open(a.len() <= 12).show(ui, |ui| {
        for (i, x) in a.iter_mut().enumerate().take(LIST_ROWS) {
            let p = format!("{path}.{i}");
            let what = match x {
                Value::Array(p) if is_point(p) => "Point",
                Value::Array(_) => "Loop",
                _ => "Item",
            };
            let l = format!("{what} {}", i + 1);
            ui.horizontal(|ui| {
                if crate::ui_kit::button(ui, "✕", crate::ui_kit::Button::Small).on_hover_text(solidcraft_ui_text::tr_label("Remove")).clicked() {
                    remove = Some(i);
                }
                ui.vertical(|ui| value(ui, inputs, &p, &l, x, None));
            });
        }
        if a.len() > LIST_ROWS {
            ui.label(RichText::new(format!("{} more in the JSON tab", a.len() - LIST_ROWS)).color(t.text_dim));
        }
        if crate::ui_kit::button(ui, "Add", crate::ui_kit::Button::Small).clicked() {
            add = true;
        }
    });
    if remove.is_some() || add {
        if let Some(i) = remove.filter(|i| *i < a.len()) {
            a.remove(i);
        }
        if add {
            a.push(new_item(path, a.last()));
        }
        // Rows moved: their typed text belongs to other items now.
        let prefix = format!("{path}.");
        inputs.text.retain(|k, _| !k.starts_with(&prefix));
        inputs.invalid.retain(|k| !k.starts_with(&prefix));
    }
}

/// The item Add appends: a copy of the last, else an empty loop for `loops` or a point.
fn new_item(path: &str, last: Option<&Value>) -> Value {
    if path.ends_with("bodies") || path.ends_with("fixtures") || path.ends_with("collets") {
        return match last {
            Some(x) => x.clone(),
            None if path.ends_with("collets") => json!(3.175),
            None => json!(""),
        };
    }
    match last {
        Some(Value::Array(p)) if !is_point(p) => json!([]),
        Some(x) => x.clone(),
        None if path.ends_with("loops") => json!([]),
        None if path.contains("loops.") => json!([0.0, 0.0]),
        None => json!([0.0, 0.0, 0.0]),
    }
}

/// A text field holding `shown` until typed in; returns the parsed value once the text is valid.
fn typed(
    ui: &mut egui::Ui,
    inputs: &mut Inputs,
    path: &str,
    label: &str,
    shown: &str,
    hint: &str,
    parse: impl Fn(&str) -> Option<Value>,
) -> Option<Value> {
    let text = inputs.text.entry(path.to_string()).or_insert_with(|| shown.to_string());
    let changed = crate::ui_kit::row(ui, label, |ui| {
        let r = ui.scope(|ui| crate::ui_kit::text_field(ui, text, hint).1);
        r.response.on_hover_text(path);
        r.inner
    });
    if !changed {
        return None;
    }
    match parse(text) {
        Some(v) => {
            inputs.invalid.remove(path);
            Some(v)
        }
        None => {
            inputs.invalid.insert(path.to_string());
            None
        }
    }
}

/// A height: what it is measured from, and the offset from there.
fn height(ui: &mut egui::Ui, inputs: &mut Inputs, path: &str, label: &str, v: &mut Value, from: &[String]) {
    let Value::Object(h) = v else { return };
    let t = Tokens::get();
    let current = h.get("from").and_then(Value::as_str).unwrap_or("").to_string();
    let mut i = from.iter().position(|c| *c == current).unwrap_or(0);
    let labels: Vec<String> = from.iter().map(|c| kind_label(c)).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let offset = h.get("offset").map(number_text).unwrap_or_default();
    let text = inputs.text.entry(format!("{path}.offset")).or_insert(offset);
    let changed = crate::ui_kit::row(ui, label, |ui| {
        let w = ((ui.available_width() - t.space_8) / 2.0).max(0.0);
        ui.spacing_mut().item_spacing.x = t.space_8;
        ui.allocate_ui(egui::vec2(w, crate::ui_kit::row_height()), |ui| crate::ui_kit::dropdown(ui, &format!("cam_ed:{path}"), &mut i, &refs));
        ui.allocate_ui(egui::vec2(w, crate::ui_kit::row_height()), |ui| crate::ui_kit::text_field(ui, text, "offset mm").1).inner
    });
    if let Some(c) = from.get(i).filter(|c| **c != current) {
        h.insert("from".into(), json!(c));
    }
    if changed {
        let key = format!("{path}.offset");
        let parsed = inputs.text.get(&key).and_then(|s| parse_number(s, false));
        match parsed {
            Some(n) => {
                inputs.invalid.remove(&key);
                h.insert("offset".into(), n);
            }
            None => {
                inputs.invalid.insert(key);
            }
        }
    }
}

/// A choice that brings its own parameters (the ramp's kind, the geometry's source): a dropdown
/// of the choices, then the chosen one's parameters.
fn tagged(ui: &mut egui::Ui, inputs: &mut Inputs, path: &str, v: &mut Value, tag: &str, choices: &[String]) {
    tagged_with(ui, inputs, path, v, tag, choices, &[]);
}

/// [`tagged`], with fixed choices for some of the chosen kind's fields.
fn tagged_with(ui: &mut egui::Ui, inputs: &mut Inputs, path: &str, v: &mut Value, tag: &str, choices: &[String], fields: &[(&str, Vec<String>)]) {
    let Value::Object(o) = v else { return };
    let current = o.get(tag).and_then(Value::as_str).unwrap_or("").to_string();
    let mut i = choices.iter().position(|c| *c == current).unwrap_or(0);
    let labels: Vec<String> = choices.iter().map(|c| kind_label(c)).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let label = match tag {
        "source" => "Source",
        "mode" => "Stock",
        _ => "Kind",
    };
    crate::ui_kit::row(ui, label, |ui| crate::ui_kit::dropdown(ui, &format!("cam_ed:{path}.{tag}"), &mut i, &refs));
    if let Some(c) = choices.get(i).filter(|c| **c != current) {
        let mut new = tagged_default(tag, c);
        if let Value::Object(n) = &mut new {
            for (k, x) in n.iter_mut() {
                if let Some(old) = o.get(k).filter(|old| std::mem::discriminant(*old) == std::mem::discriminant(&*x)) {
                    *x = old.clone();
                }
            }
            n.insert(tag.into(), json!(c));
            *o = std::mem::take(n);
        }
        inputs.text.retain(|k, _| !k.starts_with(&format!("{path}.")));
        inputs.invalid.retain(|k| !k.starts_with(&format!("{path}.")));
    }
    let keys: Vec<String> = o.keys().filter(|k| *k != tag).cloned().collect();
    for key in keys {
        let p = format!("{path}.{key}");
        if let Some(x) = o.get_mut(&key) {
            let l = label_of(&key, x);
            let list = fields.iter().find(|(k, _)| *k == key).map(|x| x.1.as_slice());
            value(ui, inputs, &p, &l, x, list);
        }
    }
}

/// What an operation's geometry picks, in a few words (`None`: nothing, it is the stock or the
/// model's silhouette).
fn geometry_picks(g: &Value) -> Option<String> {
    let count = |k: &str| g.get(k).and_then(Value::as_array).map_or(0, Vec::len);
    let n = match g.get("source").and_then(Value::as_str)? {
        "faces" => (count("points"), "face"),
        "walls" => (count("points"), "wall"),
        "holes" => (count("points"), "hole"),
        "loops" => (count("loops"), "loop"),
        "sketch" => return Some(format!("sketch {}", g["feature"].as_u64().unwrap_or(0))),
        _ => return None,
    };
    match n.0 {
        0 => None,
        1 => Some(format!("1 {}", n.1)),
        c => Some(format!("{c} {}s", n.1)),
    }
}

/// The origin's rows in the setup form; returns what to add to `cam.setup.set` when one of its
/// buttons is pressed.
fn origin_rows(ui: &mut egui::Ui, of: &mut OriginForm, points: &[String]) -> Option<Value> {
    let t = Tokens::get();
    let mut out = None;
    ui.add_space(t.space_8);
    ui.label(RichText::new("Place the origin").strong());
    crate::ui_kit::row(ui, "On", |ui| crate::ui_kit::dropdown(ui, "cam_ed_origin_of", &mut of.of, &["Stock", "Model"]));
    crate::ui_kit::row(ui, "X", |ui| crate::ui_kit::dropdown(ui, "cam_ed_origin_x", &mut of.x, &["Left", "Center", "Right"]));
    crate::ui_kit::row(ui, "Y", |ui| crate::ui_kit::dropdown(ui, "cam_ed_origin_y", &mut of.y, &["Front", "Center", "Back"]));
    crate::ui_kit::row(ui, "Z", |ui| crate::ui_kit::dropdown(ui, "cam_ed_origin_z", &mut of.z, &["Top", "Middle", "Bottom"]));
    let on = ["stock", "model"].get(of.of).copied().unwrap_or("stock");
    if crate::ui_kit::button(ui, "Place Origin", crate::ui_kit::Button::Normal)
        .on_hover_text("Moves the origin to that point of the stock's or the model's box, in the setup's axes")
        .clicked()
    {
        fn pick(i: usize, names: [&'static str; 3]) -> &'static str {
            names.get(i).copied().unwrap_or(names[0])
        }
        out = Some(json!({"origin_at": {
            "of": on,
            "x": pick(of.x, ["left", "center", "right"]),
            "y": pick(of.y, ["front", "center", "back"]),
            "z": pick(of.z, ["top", "middle", "bottom"]),
        }}));
    }
    // Points the designer placed for it.
    if !points.is_empty() {
        let refs: Vec<&str> = points.iter().map(String::as_str).collect();
        of.point = of.point.min(points.len() - 1);
        crate::ui_kit::row(ui, "Design point", |ui| crate::ui_kit::dropdown(ui, "cam_ed_origin_point", &mut of.point, &refs));
        if crate::ui_kit::button(ui, "Use Point", crate::ui_kit::Button::Normal).clicked()
            && let Some(name) = points.get(of.point)
        {
            out = Some(json!({"origin": {"point": name}}));
        }
    }
    // Picked on the model or the stock: it snaps to vertices, edge midpoints, circle and face
    // centres, and the stock's corners, edge midpoints, face centres and centre.
    let mut show = !of.hide_stock;
    crate::ui_kit::checkbox(ui, "Show Stock", &mut show).on_hover_text("Draws the setup's stock, so its points can be picked");
    of.hide_stock = !show;
    let label = if of.picking { "Stop Picking" } else { "Pick on Model or Stock" };
    if crate::ui_kit::button(ui, label, crate::ui_kit::Button::Normal).clicked() {
        of.picking = !of.picking;
        of.before = None;
        of.hover = None;
        of.current = None;
        of.chosen = None;
    }
    if of.picking {
        let stock = if of.hide_stock { "" } else { ", and to the stock's corners, edge midpoints, face centres and centre" };
        ui.label(
            RichText::new(format!(
                "Move over the model: the origin snaps to vertices, edge midpoints and circle and face centres{stock}. Click to place it; Esc stops."
            ))
            .color(t.warning),
        );
    }
    out
}

/// While a setup is edited: its stock, drawn over the model; while its origin is being picked,
/// the snap under the pointer (of the model or the stock, the nearer on screen), and a click on
/// it (called by the viewport after the model is drawn).
pub fn overlay(app: &mut SolveApp, painter: &egui::Painter, proj: &crate::viewport::Proj) {
    use crate::dialogs_assembly::{SnapPt, glyph, snap_name};
    let Some(mut ed) = app.cam_editor.take() else { return };
    let t = Tokens::get();
    let stock = match ed.target {
        Target::Setup(id) if !ed.origin.hide_stock => {
            // Worked out again only when the design (its bodies, what is shown, the setups) changes.
            let key = (id, app.session.design_revision());
            match &ed.stock_cache {
                Some((k, s)) if *k == key => *s,
                _ => {
                    let s = solidcraft_engine::cmd::cam::setup_stock(&app.session, id);
                    ed.stock_cache = Some((key, s));
                    s
                }
            }
        }
        _ => None,
    };
    if let Some((shape, wcs)) = &stock {
        draw_stock(painter, proj, shape, wcs, t.text);
    }
    if ed.origin.picking {
        let ctx = painter.ctx().clone();
        let pointer = ctx
            .input(|i| i.pointer.hover_pos())
            .filter(|p| proj.rect.contains(*p) && !crate::shell::covered(app, *p) && ctx.layer_id_at(*p).is_none_or(|l| l == painter.layer_id()));
        let under = app.viewport.hover.as_ref().and_then(crate::viewport::hit_sel);
        match &under {
            Some(sel) => {
                if !matches!(&ed.origin.hover, Some((s, _)) if s == sel) {
                    ed.origin.hover = Some((sel.clone(), crate::dialogs_assembly::snap_of(&app.session, sel)));
                }
            }
            None => ed.origin.hover = None,
        }
        let model = ed.origin.hover.as_ref().and_then(|h| h.1.clone());
        let on_stock = pointer.zip(stock.as_ref()).and_then(|(p, (shape, wcs))| nearest_stock_point(proj, shape, wcs, p, 14.0));
        let dist = |a: solidcore_record::Vec3| pointer.zip(proj.to_screen(a)).map_or(f32::INFINITY, |(p, q)| p.distance(q));
        let pick: Option<(SnapPt, &str)> = match (model, on_stock) {
            (Some(m), Some(s)) if dist(s.0.at) < dist(m.at) => Some(s),
            (Some(m), _) => {
                let name = snap_name(m.kind);
                Some((m, name))
            }
            (None, s) => s,
        };
        ed.origin.current = pick.as_ref().map(|p| p.0.at);
        if let Some((s, name)) = &pick {
            glyph(painter, proj, s, t.accent, app.cam.half_height(), Some(name));
        }
        if pointer.is_some() && ctx.input(|i| i.pointer.primary_clicked()) && ed.origin.current.is_some() {
            ed.origin.chosen = ed.origin.current;
        }
    }
    app.cam_editor = Some(ed);
}

/// The stock's outline (in the text colour, apart from the toolpaths' accent): a box's twelve
/// edges, or a cylinder's end circles and four lines.
fn draw_stock(painter: &egui::Painter, proj: &crate::viewport::Proj, shape: &cam::setup::StockShape, wcs: &cam::frame::Frame, col: egui::Color32) {
    use solidcore_record::Vec3;
    let stroke = egui::Stroke::new(1.5, col.gamma_multiply(0.85));
    let line = |a: Vec3, b: Vec3| {
        if let (Some(p), Some(q)) = (proj.to_screen(wcs.to_world(a)), proj.to_screen(wcs.to_world(b))) {
            painter.line_segment([p, q], stroke);
        }
    };
    match *shape {
        cam::setup::StockShape::Box { min, max } => {
            let c = |i: usize| {
                Vec3::new(if i & 1 == 0 { min.x } else { max.x }, if i & 2 == 0 { min.y } else { max.y }, if i & 4 == 0 { min.z } else { max.z })
            };
            for (a, b) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
                line(c(a), c(b));
            }
        }
        cam::setup::StockShape::Cylinder { axis, radius, start, end } => {
            let d = axis.dir();
            let (u, v) = match axis {
                cam::setup::Axis::X => (Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0)),
                cam::setup::Axis::Y => (Vec3::new(0.0, 0.0, 1.0), Vec3::new(1.0, 0.0, 0.0)),
                cam::setup::Axis::Z => (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)),
            };
            let at = |along: f64, a: f64| d * along + (u * a.cos() + v * a.sin()) * radius;
            let n = 48;
            for k in 0..n {
                let (a, b) = (k as f64 / n as f64 * std::f64::consts::TAU, (k + 1) as f64 / n as f64 * std::f64::consts::TAU);
                line(at(start, a), at(start, b));
                line(at(end, a), at(end, b));
            }
            for k in 0..4 {
                let a = k as f64 * std::f64::consts::FRAC_PI_2;
                line(at(start, a), at(end, a));
            }
        }
    }
}

/// The stock's point nearest the pointer on screen, within `reach` pixels: its corners, edge
/// midpoints, face centres and centre (of its box; a cylinder's end centres and centre).
fn nearest_stock_point(
    proj: &crate::viewport::Proj,
    shape: &cam::setup::StockShape,
    wcs: &cam::frame::Frame,
    pointer: egui::Pos2,
    reach: f32,
) -> Option<(crate::dialogs_assembly::SnapPt, &'static str)> {
    use crate::dialogs_assembly::{SnapKind, SnapPt};
    use solidcore_record::Vec3;
    let mut pts: Vec<(Vec3, SnapKind, &'static str)> = Vec::new();
    match *shape {
        cam::setup::StockShape::Box { min, max } => {
            let f = |lo: f64, hi: f64, i: usize| lo + (hi - lo) * i as f64 * 0.5;
            for i in 0..3 {
                for j in 0..3 {
                    for k in 0..3 {
                        // How many of the three are at a middle: 0 a corner, 1 an edge midpoint,
                        // 2 a face centre, 3 the centre.
                        let mid = [i, j, k].iter().filter(|x| **x == 1).count();
                        let (kind, name) = match mid {
                            0 => (SnapKind::Vertex, "Stock corner"),
                            1 => (SnapKind::EdgeMid, "Stock edge midpoint"),
                            2 => (SnapKind::FaceCenter, "Stock face center"),
                            _ => (SnapKind::FaceCenter, "Stock center"),
                        };
                        pts.push((Vec3::new(f(min.x, max.x, i), f(min.y, max.y, j), f(min.z, max.z, k)), kind, name));
                    }
                }
            }
        }
        cam::setup::StockShape::Cylinder { axis, start, end, .. } => {
            let d = axis.dir();
            pts.push((d * start, SnapKind::CircleCenter, "Stock end center"));
            pts.push((d * end, SnapKind::CircleCenter, "Stock end center"));
            pts.push((d * ((start + end) * 0.5), SnapKind::FaceCenter, "Stock center"));
        }
    }
    let z = wcs.dir_to_world(Vec3::new(0.0, 0.0, 1.0));
    pts.into_iter()
        .filter_map(|(p, kind, name)| {
            let w = wcs.to_world(p);
            let d = proj.to_screen(w)?.distance(pointer);
            (d.is_finite() && d <= reach).then_some((d, SnapPt { at: w, z, kind, param: Value::Null }, name))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, s, name)| (s, name))
}

/// The parameters a ramp kind or geometry source starts with.
fn tagged_default(tag: &str, choice: &str) -> Value {
    match (tag, choice) {
        ("type", "helix") => json!({"diameter": 0.0, "angle_deg": 2.0}),
        ("type", "zigzag" | "profile") => json!({"angle_deg": 2.0}),
        ("source", "loops") => json!({"loops": []}),
        ("source", "sketch") => json!({"feature": 0}),
        ("source", "faces") => json!({"points": []}),
        ("source", "holes") => json!({"points": [], "diameter": 0.0}),
        ("source", "chains") => json!({"bottom": [], "top": []}),
        ("source", "rails") => json!({"first": [], "second": []}),
        ("mode", "relative_box") => json!({"x_neg": 1.0, "x_pos": 1.0, "y_neg": 1.0, "y_pos": 1.0, "z_neg": 0.0, "z_pos": 1.0}),
        ("mode", "fixed_box") => json!({"min": [0.0, 0.0, 0.0], "max": [100.0, 100.0, 20.0]}),
        ("mode", "relative_cylinder") => json!({"axis": "x", "radial": 1.0, "start": 0.0, "end": 0.0}),
        ("mode", "fixed_cylinder") => json!({"axis": "x", "radius": 10.0, "start": 0.0, "end": 50.0}),
        ("mode", "from_setup") => json!({"setup": 0}),
        ("kind", "automatic") => json!({"slots": 6}),
        _ => json!({}),
    }
}

/// Changes the operation's kind: the old kind's parameters go, the new one's come at their
/// defaults; name, tool, geometry, heights and linking stay.
fn change_kind(op: &mut Map<String, Value>, ty: &str) {
    let Some(Value::Object(d)) = cam::ops::OpKind::defaults(ty) else { return };
    op.retain(|k, _| COMMON.contains(&k.as_str()));
    for (k, v) in d {
        op.insert(k, v);
    }
}

/// A key as a label, with its unit: `stay_down_distance` is "Stay down distance (mm)".
fn label_of(key: &str, v: &Value) -> String {
    let unit = unit_of(key, v);
    let bare = ["_deg", "_s", "_mm", "_nm"].iter().find_map(|u| key.strip_suffix(u)).unwrap_or(key);
    let words = kind_label(bare);
    if unit.is_empty() { words } else { format!("{words} ({unit})") }
}

/// The unit a parameter is in, from its name: lengths are mm, `_deg` degrees, `_s` seconds.
fn unit_of(key: &str, v: &Value) -> &'static str {
    if !v.is_f64() {
        return "";
    }
    if key.ends_with("_deg") {
        "°"
    } else if key.ends_with("_nm") {
        "nm"
    } else if key.ends_with("_s") {
        "s"
    } else if key == "power" {
        "0–1"
    } else if key == "speed" {
        "mm/min"
    } else {
        "mm"
    }
}

/// `steep_shallow` as "Steep shallow", `pocket2d` as "Pocket 2D".
fn kind_label(name: &str) -> String {
    let spaced = name.replace('_', " ").replace("2d", " 2D").replace("3d", " 3D");
    let mut c = spaced.trim().chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn number_text(v: &Value) -> String {
    match v.as_f64() {
        Some(x) if v.is_f64() => {
            let s = format!("{x:.6}");
            let s = s.trim_end_matches('0').trim_end_matches('.');
            if s == "-0" { "0".into() } else { s.to_string() }
        }
        _ => v.to_string(),
    }
}

/// A typed number: a whole number for a count, else a decimal.
fn parse_number(s: &str, integer: bool) -> Option<Value> {
    let x = crate::ui_kit::parse_number(s.trim().trim_end_matches("mm").trim_end_matches('°'))?;
    if integer { (x.fract() == 0.0 && (0.0..=1e9).contains(&x)).then(|| json!(x as u64)) } else { Some(json!(x)) }
}

/// A point as saved: `[x, y]` or `[x, y, z]`.
fn is_point(a: &[Value]) -> bool {
    (2..=3).contains(&a.len()) && a.iter().all(Value::is_number)
}

fn point_text(a: &[Value]) -> String {
    a.iter().map(number_text).collect::<Vec<_>>().join(", ")
}

/// "x, y" or "x, y, z" (spaces or semicolons also separate); an empty text is not set.
fn parse_point(s: &str) -> Option<Value> {
    let s = s.trim();
    if s.is_empty() {
        return Some(Value::Null);
    }
    let parts: Vec<f64> = s.split([',', ';', ' ']).filter(|p| !p.is_empty()).map(crate::ui_kit::parse_number).collect::<Option<_>>()?;
    match parts.as_slice() {
        [x, y] => Some(json!([x, y])),
        [x, y, z] => Some(json!([x, y, z])),
        _ => None,
    }
}

#[cfg(test)]
#[path = "cam_editor_tests.rs"]
mod tests;
