//! The Manufacture workspace: the MANUFACTURE ribbon tab with the `cam.*` commands, a panel
//! that shows the manufacturing model (tools, setups, operations, what needs attention) and
//! the machining simulation. Like Render, it is a ribbon tab the design tabs give way to; the
//! viewport, history and selection stay the design's.

use egui::{Color32, RichText};
use serde_json::{Value, json};

use crate::SolveApp;
use solidcraft_theme::Tokens;

pub const TAB: &str = "MANUFACTURE";

pub fn active(app: &SolveApp) -> bool {
    app.ui.tab == TAB
}

/// Enter (`on`) or leave the workspace. Leaving ends a simulation preview.
pub fn enter(app: &mut SolveApp, on: bool) -> Result<(), String> {
    if on {
        if app.session.active_sketch.is_some() || app.tool.is_some() || app.dialog.is_some() || app.file_load.busy() {
            return Err("finish the current modelling operation before entering Manufacture".into());
        }
        if crate::render_workspace::active(app) {
            crate::render_workspace::enter(app, false)?;
        }
        app.ui.tab = TAB.into();
    } else if active(app) {
        if app.session.cam.preview.is_some() {
            let _ = app.run("cam.simulate.hide", json!({}));
        }
        app.ui.tab = "MODEL".into();
    }
    app.cancel_view_animation();
    Ok(())
}

/// A simulation preview started from a script, the CLI or the agent (`cam.simulate.show`)
/// brings the workspace with it, so the simulation never plays over the design tabs.
pub fn sync(app: &mut SolveApp) {
    if !active(app)
        && app.session.cam.preview.is_some()
        && !crate::render_workspace::active(app)
        && app.session.active_sketch.is_none()
        && app.dialog.is_none()
        && app.tool.is_none()
    {
        app.ui.tab = TAB.into();
    }
}

/// A ribbon button for a `cam.*` command. The commands that need parameters have no dialog
/// yet: the button opens the command box with a filled-in call to edit and run. Returns false
/// when the command is not a CAM command (the caller goes on as usual).
pub fn start(app: &mut SolveApp, id: &str) -> bool {
    if !id.starts_with("cam.") {
        return false;
    }
    let setup = selected_setup(app);
    // The Setup panel's buttons open forms: labelled fields, dropdowns and OK / Cancel (the
    // form's JSON tab and the command box still take the raw call).
    match id {
        "cam.tool.add" => {
            crate::cam_editor::open_new_tool(app);
            return true;
        }
        // New Setup and Set Zero: two clicks (the top face, then the zero point), no form; the
        // full form is behind the panel's Advanced button.
        "cam.setup.create" => {
            crate::cam_editor::open_quick_setup(app, None);
            return true;
        }
        "cam.setup.zero" => {
            crate::cam_editor::open_quick_setup(app, setup);
            return true;
        }
        "cam.tool.kit" => {
            let mut params = json!({"kit": "makera_essential", "material": "aluminium"});
            if let Some(s) = setup {
                params["setup"] = json!(s);
            }
            crate::cam_editor::open_run(app, "cam.tool.kit", params);
            return true;
        }
        "cam.machine.set" => {
            let Some(s) = setup else {
                app.set_status("Create a setup first: its form lets you pick the machine too.".to_string(), false);
                crate::cam_editor::open_new_setup(app);
                return true;
            };
            // Start from the setup's own machine when it is a known one.
            let current = app.session.doc.manufacture.setup(s).map(|st| st.machine.name.clone()).unwrap_or_default();
            let machine = solidcraft_engine::cmd::cam::known_machines(&app.session)
                .into_iter()
                .find(|e| e.machine.name == current)
                .map(|e| e.id)
                .unwrap_or_else(|| "three_axis".to_string());
            crate::cam_editor::open_run(app, "cam.machine.set", json!({"setup": s, "machine": machine}));
            return true;
        }
        _ => {}
    }
    // An operation needs a setup to go into: with none yet, its form comes first.
    let makes_operation = id.strip_prefix("cam.operation.").is_some_and(|k| !matches!(k, "set" | "remove" | "list" | "kinds" | "move" | "duplicate"));
    if makes_operation && setup.is_none() {
        app.set_status("Create a setup first, then pick the operation again.".to_string(), false);
        crate::cam_editor::open_new_setup(app);
        return true;
    }
    // With no tool in the library the form still opens (its Tool box empty), so a button can be
    // clicked just to see what it does; OK asks for a tool.
    if makes_operation && app.session.doc.manufacture.tools.tools.is_empty() {
        app.set_status("No tools yet: add one with Add Tool, then pick it in the form's Tool box.".to_string(), false);
    }
    let m = &app.session.doc.manufacture;
    let tool = m.tools.tools.first().map(|t| t.id);
    let template = match id {
        "cam.tool.add" => Some(json!({"name": "6 mm flat", "kind": "flat", "diameter": 6, "flute_length": 20, "overall_length": 50})),
        "cam.setup.create" => Some(
            json!({"name": format!("Setup {}", m.setups.len() + 1), "machine": "carvera", "stock": {"mode": "relative_box", "x_neg": 1, "x_pos": 1, "y_neg": 1, "y_pos": 1, "z_neg": 0, "z_pos": 1}}),
        ),
        "cam.tool.kit" => Some(json!({"kit": "makera_essential", "material": "aluminium"})),
        "cam.machine.set" => Some(json!({"setup": setup, "machine": "carvera"})),
        "cam.operation.add" => {
            let tools: Vec<(u64, &str)> = m.tools.tools.iter().map(|t| (t.id, tool_class(t))).collect();
            let picked = crate::cam_editor::selection_geometry(app, "pocket3d").map(|x| x.0);
            Some(kind_template("pocket3d", setup, tool, &tools, picked))
        }
        "cam.operation.set" => Some(json!({"operation": null})),
        _ => {
            let tools: Vec<(u64, &str)> = m.tools.tools.iter().map(|t| (t.id, tool_class(t))).collect();
            id.strip_prefix("cam.operation.").filter(|k| !matches!(*k, "add" | "remove" | "list" | "kinds")).map(|k| {
                let picked = crate::cam_editor::selection_geometry(app, if k == "tap" { "drill" } else { k }).map(|x| x.0);
                kind_template(k, setup, tool, &tools, picked)
            })
        }
    };
    // A new operation opens in the operation editor (Fields and JSON), for the setup picked.
    let new_kind = match id.strip_prefix("cam.operation.") {
        Some("add") => Some("pocket3d"),
        Some("tap") => Some("drill"),
        Some("set" | "remove" | "list" | "kinds" | "move" | "duplicate") | None => None,
        Some(k) => Some(k),
    };
    if let (Some(call), Some(setup), Some(kind)) = (&template, setup, new_kind)
        && crate::cam_editor::open_new(app, setup, kind, call)
    {
        return true;
    }
    if let Some(template) = template {
        // Its form, filled in with the template (never the command box).
        if let Some(spec) = solidcraft_engine::find_command(id) {
            crate::cam_editor::open_run(app, spec.id, template);
        }
        return true;
    }
    // A CAM command that needs parameters opens its form built from them.
    if crate::auto_form::needs_form(id) && crate::cam_editor::open_command(app, id, json!({})) {
        return true;
    }
    if id == "cam.post" {
        crate::cam_editor::open_post(app, None);
        return true;
    }
    if id == "cam.generate" {
        crate::cam_job::start(app, json!({}));
        return true;
    }
    if id == "cam.simulate.show" && app.session.cam.preview.is_some() {
        // Already showing: the panel drives it.
        return true;
    }
    // Simulate needs to know which setup (SolidCraft CAM): the one picked in the Browser (or the
    // picked operation's), the only one there is, or, with several and none picked, a short form
    // with the setups to pick from.
    let mut params = json!({});
    if matches!(id, "cam.simulate.show" | "cam.simulate") {
        use crate::browser::cam::CamPick;
        let m = &app.session.doc.manufacture;
        let picked = match app.tree.picked_cam {
            Some(CamPick::Setup(s)) => Some(s),
            Some(CamPick::Operation(o)) => m.operation(o).map(|(st, _)| st.id),
            _ => None,
        }
        .filter(|s| m.setup(*s).is_some());
        match (picked, m.setups.as_slice()) {
            (Some(s), _) => params = json!({"setup": s}),
            (None, [only]) => params = json!({"setup": only.id}),
            (None, [first, ..]) => {
                let first = first.id;
                if let Some(spec) = solidcraft_engine::find_command(id) {
                    crate::cam_editor::open_run(app, spec.id, json!({"setup": first}));
                }
                return true;
            }
            (None, []) => {}
        }
    }
    match app.run(id, params) {
        Ok(v) => {
            if let Some(info) = v.get("notice").and_then(|n| n.get("info")).and_then(Value::as_str) {
                app.set_status(info.to_string(), v["notice"]["status"] == "error");
            }
        }
        // Not an error, just a step missing: said plainly.
        Err(e) if e.contains("run cam.generate first") => {
            app.set_status("Nothing to simulate yet: press Generate first, then Simulate.".to_string(), false)
        }
        Err(e) => app.set_status(e, true),
    }
    true
}

/// The setup new operations go into: the one picked in the Browser (or the picked operation's),
/// else the first one.
pub fn selected_setup(app: &SolveApp) -> Option<u64> {
    use crate::browser::cam::CamPick;
    let m = &app.session.doc.manufacture;
    let picked = match app.tree.picked_cam {
        Some(CamPick::Setup(s)) => Some(s),
        Some(CamPick::Operation(o)) => m.operation(o).map(|(st, _)| st.id),
        _ => None,
    };
    picked.filter(|s| m.setup(*s).is_some()).or_else(|| m.setups.first().map(|s| s.id))
}

/// A filled-in `cam.operation.<kind>` call: the kind's parameters at their defaults and the
/// geometry the kind reads (`picked`: from the viewport's selection, when something is selected),
/// for the command box.
/// What a tool is for: "laser", "plasma" or "cutter", so a new operation starts with a tool of
/// its own kind.
fn tool_class(t: &solidcraft_engine::cam::tool::Tool) -> &'static str {
    if t.is_laser() {
        "laser"
    } else if t.is_plasma() {
        "plasma"
    } else {
        "cutter"
    }
}

fn kind_template(kind: &str, setup: Option<u64>, tool: Option<u64>, tools: &[(u64, &str)], picked: Option<Value>) -> Value {
    use solidcraft_engine::cam::ops::OpKind;
    let (kind_name, extra): (&str, Value) = match kind {
        "tap" => ("drill", json!({"cycle": "tap", "pitch": 1.0})),
        other => (other, json!({})),
    };
    let mut params =
        serde_json::from_value::<OpKind>(json!({"type": kind_name})).ok().and_then(|k| serde_json::to_value(k).ok()).unwrap_or(json!({}));
    if let Value::Object(o) = &mut params {
        o.remove("type");
        if let Value::Object(e) = extra {
            for (k, v) in e {
                o.insert(k, v);
            }
        }
    }
    let geometry = match kind_name {
        "face" => json!({"source": "stock"}),
        "pocket2d" | "adaptive2d" | "chamfer2d" => json!({"source": "faces", "points": [[0, 0, 0]]}),
        "contour2d" | "project" => json!({"source": "silhouette"}),
        "slot" | "trace" | "engrave" => json!({"source": "sketch", "feature": 0}),
        "drill" | "bore" | "circular" | "thread" => json!({"source": "holes", "points": [[0, 0, 0]]}),
        "swarf" => json!({"source": "chains", "bottom": [[0, 0, 0], [10, 0, 0]], "top": [[0, 0, 10], [10, 0, 10]]}),
        "flow" | "morph" => json!({"source": "rails", "first": [[0, 0, 0], [10, 0, 0]], "second": [[0, 10, 0], [10, 10, 0]]}),
        "laser_cut" | "plasma_cut" => json!({"source": "sketch", "feature": 0}),
        "laser_engrave" => json!({"source": "faces", "points": [[0, 0, 0]]}),
        _ => Value::Null,
    };
    // Something selected: the operation works on it (and an area operation machines within it).
    let bounded = serde_json::from_value::<OpKind>(json!({"type": kind_name})).is_ok_and(|k| k.uses_boundary());
    let geometry = match picked {
        Some(g) if !geometry.is_null() || bounded => g,
        _ => geometry,
    };
    // Laser work wants the laser tool, turning a vertical cutter: pick the library's first of
    // the kind when there is one.
    let tool = match kind_name {
        "laser_cut" | "laser_engrave" => tools.iter().find(|t| t.1 == "laser").map(|t| t.0).or(tool),
        "plasma_cut" => tools.iter().find(|t| t.1 == "plasma").map(|t| t.0).or(tool),
        _ => tools.iter().find(|t| t.1 == "cutter").map(|t| t.0).or(tool),
    };
    // No tool yet: 0 (no tool has id 0), so the form opens with its Tool box empty.
    let mut call = json!({"setup": setup, "tool": tool.unwrap_or(0), "name": kind_name});
    if let (Value::Object(c), Value::Object(p)) = (&mut call, params) {
        for (k, v) in p {
            c.insert(k, v);
        }
        if !geometry.is_null() {
            c.insert("geometry".into(), geometry);
        }
    }
    call
}

fn status_color(t: &Tokens, status: &str) -> Color32 {
    match status {
        "ok" => t.agent_ok,
        "warning" => t.warning,
        "error" => t.error,
        _ => t.text_dim,
    }
}

/// The panel's width, margins included.
const PANEL_WIDTH: f32 = 305.0;
/// The id the panel's footprint is registered under, so the shared host docks
/// its panels and places the view cube and overlays beside it.
const PANEL_ID: &str = "manufacture:panel";

/// Where the panel's folded state is kept (with the window layout, so it lasts between runs).
fn folded_id() -> egui::Id {
    egui::Id::unique("sc_manufacture_panel_folded")
}

/// Whether the panel is folded down to its Show button, leaving the viewport whole.
fn folded(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.get_persisted::<bool>(folded_id())).unwrap_or(false)
}

fn set_folded(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_persisted(folded_id(), on));
}

/// The folded panel: a button at the top right that opens it again.
const FOLDED_SIZE: egui::Vec2 = egui::vec2(170.0, 44.0);

/// The right-hand strip the panel takes, between the ribbon and the status bar (folded, just
/// its Show button's corner).
fn panel_rect(app: &SolveApp, ctx: &egui::Context, domain: egui::Rect) -> egui::Rect {
    let full = full_panel_rect(app, ctx, domain);
    if folded(ctx) {
        let left = (full.right() - FOLDED_SIZE.x).max(full.left());
        let bottom = (full.top() + FOLDED_SIZE.y).min(full.bottom());
        return egui::Rect::from_min_max(egui::pos2(left, full.top()), egui::pos2(full.right(), bottom));
    }
    full
}

fn full_panel_rect(app: &SolveApp, ctx: &egui::Context, domain: egui::Rect) -> egui::Rect {
    let gap = crate::ui_kit::Container::new(ctx, crate::ui_kit::Surface::Panel).gap();
    let mut area = domain;
    if crate::status_bar::shown(app) {
        area.max.y -= crate::status_bar::HEIGHT.min(area.height().max(0.0));
    }
    if !app.embedded {
        let cards = crate::toolbar::shell_cards(app, area);
        area.min.y = cards.bottom.min(area.bottom()).max(area.top());
    }
    let top = (area.top() + gap).min(area.bottom());
    let bottom = (area.bottom() - gap).max(top);
    let right = (area.right() - gap).max(area.left());
    let left = (right - PANEL_WIDTH).max(area.left());
    egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, bottom))
}

/// Reserve the panel's footprint before the shared host lays out its panels,
/// so docked panels, the view cube and framing keep clear of it.
pub fn reserve(app: &mut SolveApp, ctx: &egui::Context, domain: egui::Rect) {
    let rect = active(app).then(|| panel_rect(app, ctx, domain));
    crate::shell::register_panel(app, PANEL_ID, rect);
}

/// The right-hand panel: the manufacturing model as it stands.
pub fn panel(app: &mut SolveApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    let mut run: Option<&'static str> = None;
    // A setup whose editor (stock, origin, machine) a button asked for.
    let mut edit_setup: Option<u64> = None;
    let Some(rect) = app.shell.workspace_panels.get(PANEL_ID).copied() else { return };
    let frame = crate::ui_kit::Container::new(ui.ctx(), crate::ui_kit::Surface::Panel).edge_frame(t.panel);
    let mut host = ui.new_child(egui::UiBuilder::new().id_salt("sc_manufacture").max_rect(rect));
    host.set_clip_rect(rect);
    crate::ui_kit::Container::paint_frame(host.painter(), frame, rect);
    // Folded: only a button that opens the panel again.
    if folded(ui.ctx()) {
        let mut inner = host.new_child(egui::UiBuilder::new().max_rect(rect.shrink(6.0)));
        inner.centered_and_justified(|ui| {
            if crate::ui_kit::button(ui, format!("◀  {}", solidcraft_ui_text::tr_label("Show Manufacture")), crate::ui_kit::Button::Normal)
                .on_hover_text(solidcraft_ui_text::tr_label("Open the Manufacture panel"))
                .clicked()
            {
                set_folded(ui.ctx(), false);
            }
        });
        return;
    }
    let inner = rect.shrink(10.0);
    let mut body = host.new_child(egui::UiBuilder::new().max_rect(inner));
    body.set_clip_rect(inner);
    body.set_width(inner.width());
    // Long tool and operation names end in an ellipsis inside the panel.
    body.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
    {
        let ui = &mut body;
        {
            crate::ui_kit::scope(ui, |ui| {
                // The heading, with Hide at its end: folds the panel away to see more of the model.
                ui.horizontal(|ui| {
                    ui.heading(solidcraft_ui_text::tr_label("Manufacture"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::ui_kit::button(ui, format!("{}  ▶", solidcraft_ui_text::tr_label("Hide")), crate::ui_kit::Button::Normal)
                            .on_hover_text(solidcraft_ui_text::tr_label("Fold the Manufacture panel away"))
                            .clicked()
                        {
                            set_folded(ui.ctx(), true);
                        }
                    });
                });
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let s = &app.session;
                    let m = &s.doc.manufacture;
                    if m.tools.tools.is_empty() && m.setups.is_empty() {
                        ui.add_space(t.space_8);
                        ui.label(RichText::new(solidcraft_ui_text::tr_label("No tools or setups yet.")).color(t.text_dim));
                        ui.small(solidcraft_ui_text::tr_label(
                            "Add a tool and create a setup, add operations, then generate, simulate and post. Every button opens its command filled in; the command box, scripts and the agent run the same cam.* commands.",
                        ));
                        return;
                    }
                    ui.add_space(t.space_8);
                    // A long tool library would push the setups off the panel: the list folds
                    // (closed by default; egui remembers the choice).
                    let tools_title = format!("{} ({})", solidcraft_ui_text::tr_label("Tools"), m.tools.tools.len());
                    egui::CollapsingHeader::new(RichText::new(tools_title).strong()).id_salt("sc_manufacture_tools").default_open(false).show(ui, |ui| {
                        for tool in &m.tools.tools {
                            let kind =
                                serde_json::to_value(tool.kind).ok().and_then(|v| v["kind"].as_str().map(str::to_string)).unwrap_or_default();
                            ui.label(format!("T{}  {}  Ø{} {}", tool.number, tool.name, tool.diameter, kind.replace('_', " ")));
                        }
                    });
                    for st in &m.setups {
                        ui.add_space(t.space_8);
                        ui.label(RichText::new(format!("{}  ·  {} ({} axes)", st.name, st.machine.name, st.machine.axis_count())).strong());
                        if crate::ui_kit::button(ui, solidcraft_ui_text::tr_label("Edit setup and machine").to_string(), crate::ui_kit::Button::Normal)
                            .on_hover_text(solidcraft_ui_text::tr_label("Open this setup's form: stock, origin, and the machine's numbers"))
                            .clicked()
                        {
                            edit_setup = Some(st.id);
                        }
                        if st.operations.is_empty() {
                            ui.label(RichText::new(solidcraft_ui_text::tr_label("No operations.")).color(t.text_dim));
                        }
                        for op in &st.operations {
                            let made = s.cam.operations.get(&op.id);
                            let state = crate::browser::cam::op_state(app, op.id);
                            let (status, stale) = match made {
                                Some(g) => (g.notice.status(), state == "stale"),
                                None if op.enabled => ("not generated", false),
                                None => ("disabled", false),
                            };
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("●").color(status_color(&t, status)));
                                // The status tag stays whole at the row's end; the name gives way.
                                let word = if stale { "stale" } else { status };
                                let tag = egui::WidgetText::from(RichText::new(solidcraft_ui_text::tr_label(word)).color(t.text_dim).small())
                                    .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Small);
                                let room = (ui.available_width() - tag.size().x - ui.spacing().item_spacing.x).max(0.0);
                                ui.scope(|ui| {
                                    ui.set_max_width(room);
                                    ui.add(egui::Label::new(format!("{}  {}", op.name, op.kind.type_name())).truncate());
                                });
                                ui.add(egui::Label::new(tag));
                            });
                            if let Some(g) = made {
                                for issue in g.notice.issues.iter().filter(|i| i.severity != solidcraft_engine::cam::notice::Severity::Info) {
                                    ui.indent(("issue", op.id, &issue.code), |ui| {
                                        ui.label(RichText::new(&issue.message).color(status_color(&t, &format!("{:?}", issue.severity).to_lowercase())).small());
                                    });
                                }
                            }
                        }
                    }
                    ui.add_space(t.space_12);
                    ui.horizontal_wrapped(|ui| {
                        for (label, id) in [("Generate", "cam.generate"), ("Simulate", "cam.simulate.show"), ("Post", "cam.post")] {
                            if crate::ui_kit::button(ui, solidcraft_ui_text::tr_label(label), crate::ui_kit::Button::Small).clicked() {
                                run = Some(id);
                            }
                        }
                    });
                    if let Some(p) = &s.cam.preview {
                        ui.add_space(t.space_8);
                        ui.small(format!("{}: {:.0} %", solidcraft_ui_text::tr_label("Simulation"), p.progress * 100.0));
                    }
                });
            });
        }
    }
    if let Some(id) = run {
        app.start(id);
    }
    if let Some(sid) = edit_setup {
        crate::browser::cam::edit_setup(app, sid);
    }
}
