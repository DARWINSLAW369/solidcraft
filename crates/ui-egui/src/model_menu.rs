//! The Model tab's click menu (SolidCraft CAM, version 20): click an edge, a face, a body or a
//! sketch profile in the view, with no tool running, and a small menu beside the pointer lists the
//! tools that fit what is selected (an edge: Fillet, Chamfer; a flat face: Sketch, Press Pull,
//! Hole …; a round face: Thread, Axis, a vertical or horizontal tangent plane …). Shift-click
//! adds to the selection and the menu follows. Picking a tool starts it with the selection, the
//! way a tool started with something selected always takes it; the tangent planes are made at
//! once.

use egui::RichText;
use serde_json::json;
use solidcraft_engine::Sel;
use solidcraft_viewport::vec::Vec3Ops as _;
use solidcraft_theme::Tokens;

use crate::SolveApp;

/// What the selection is, for the tools that fit it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Picked {
    Edges,
    FlatFaces,
    /// Round faces of one cylinder kind (holes, bosses, shafts): its tangent planes are offered.
    RoundFace,
    OtherFaces,
    Bodies,
    MeshBody,
    Profiles,
    /// Placed parts of an assembly (occurrences): joints, moving, grounding.
    Parts,
}

/// A menu row: a tool to start, or a tangent plane made at once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Row {
    Start(&'static str),
    Tangent { vertical: bool },
}

/// The rows for what is picked, in the order a person reaches for them.
pub(crate) fn rows(picked: Picked, count: usize) -> Vec<Row> {
    use Row::Start as S;
    let mut v = match picked {
        Picked::Edges => vec![S("solid.fillet"), S("solid.chamfer"), S("construct.axis")],
        Picked::FlatFaces => vec![
            S("sketch.create"),
            S("solid.press_pull"),
            S("solid.extrude"),
            S("solid.hole"),
            S("solid.shell"),
            S("solid.offset_face"),
            S("solid.draft"),
            S("construct.plane"),
            S("face.delete"),
        ],
        Picked::RoundFace => vec![
            Row::Tangent { vertical: true },
            Row::Tangent { vertical: false },
            S("construct.axis"),
            S("solid.thread"),
            S("solid.press_pull"),
            S("solid.offset_face"),
            S("construct.plane.tangent"),
            S("face.delete"),
        ],
        Picked::OtherFaces => vec![S("solid.press_pull"), S("solid.offset_face"), S("construct.plane.tangent"), S("face.delete")],
        Picked::Bodies => vec![
            S("solid.move"),
            S("solid.mirror"),
            S("solid.pattern.rectangular"),
            S("solid.pattern.circular"),
            S("solid.combine"),
            S("solid.shell"),
            S("solid.split_body"),
            S("component.from_bodies"),
        ],
        Picked::MeshBody => vec![S("mesh.convert"), S("solid.move"), S("solid.mirror")],
        Picked::Profiles => vec![S("solid.extrude"), S("solid.revolve"), S("solid.sweep"), S("solid.loft")],
        Picked::Parts => vec![
            S("joint.create"),
            S("joint.as_built"),
            S("occurrence.move"),
            S("occurrence.ground"),
            S("joint.rigid_group"),
            S("inspect.interference"),
            S("inspect.measure"),
            S("component.properties"),
        ],
    };
    // Version 21: Assemble and Inspect after the modelling tools (so the ribbon's hover marks
    // stay on those): a joint from a face or an edge, measuring anything, a section on a flat face.
    match picked {
        Picked::Edges | Picked::RoundFace => v.extend([S("joint.create"), S("inspect.measure")]),
        Picked::FlatFaces => v.extend([S("joint.create"), S("inspect.measure"), S("inspect.section")]),
        Picked::OtherFaces | Picked::MeshBody => v.push(S("inspect.measure")),
        Picked::Bodies => v.extend([S("inspect.measure"), S("inspect.center_of_mass"), S("inspect.interference")]),
        Picked::Profiles | Picked::Parts => {}
    }
    // What takes one thing only, or two or more.
    if count > 1 {
        v.retain(|r| {
            !matches!(
                r,
                Row::Tangent { .. }
                    | Row::Start("sketch.create" | "construct.plane" | "construct.axis" | "construct.plane.tangent" | "inspect.section")
            )
        });
    } else {
        v.retain(|r| !matches!(r, Row::Start("solid.combine" | "solid.loft" | "inspect.interference" | "joint.rigid_group")));
    }
    v.retain(|r| match r {
        Row::Start(id) => solidcraft_engine::find_command(id).is_some(),
        Row::Tangent { .. } => true,
    });
    v
}

/// What the selection is (all of it the same kind; a mixed selection goes by its last item).
pub(crate) fn picked(app: &SolveApp) -> Option<(Picked, usize)> {
    use solidcraft_engine::kernel::{self, FaceSurface};
    let sel = &app.session.selection;
    let last = sel.last()?;
    let world = crate::open_meshing::session_world(&app.session);
    let surface = |body: &str, index: usize| {
        world.body(body).and_then(|b| kernel::guard("model menu", || Ok(kernel::face_surface(&b.body, index))).ok().flatten())
    };
    let same = |f: fn(&Sel) -> bool| sel.iter().filter(|s| f(s)).count();
    Some(match last {
        Sel::Edge { .. } => (Picked::Edges, same(|s| matches!(s, Sel::Edge { .. }))),
        Sel::Face { body, index, .. } => {
            let n = same(|s| matches!(s, Sel::Face { .. }));
            match surface(body, *index) {
                Some(FaceSurface::Plane { .. }) => (Picked::FlatFaces, n),
                Some(FaceSurface::Cylinder { .. }) => (Picked::RoundFace, n),
                _ => (Picked::OtherFaces, n),
            }
        }
        Sel::Body { name } => {
            let n = same(|s| matches!(s, Sel::Body { .. }));
            if n == 1 && world.body(name).is_some_and(|b| b.body.is_mesh()) { (Picked::MeshBody, 1) } else { (Picked::Bodies, n) }
        }
        Sel::Profile { .. } => (Picked::Profiles, same(|s| matches!(s, Sel::Profile { .. }))),
        Sel::Occurrence { .. } => (Picked::Parts, same(|s| matches!(s, Sel::Occurrence { .. }))),
        _ => return None,
    })
}

/// The Model tools that fit the edge or face under the pointer, for the ribbon to mark (the way the
/// Manufacture tab marks operations). Off the view (on the ribbon or a drop-down) it keeps what the
/// pointer was last on there; pointing at nothing in the view clears it.
pub(crate) fn hover_ids(app: &SolveApp) -> Vec<&'static str> {
    use crate::viewport::Hit;
    use solidcraft_engine::kernel::{self, FaceSurface};
    thread_local! {
        static LAST: std::cell::RefCell<(Option<(String, usize, bool)>, Vec<&'static str>)> = const { std::cell::RefCell::new((None, Vec::new())) };
    }
    let key = match &app.viewport.hover {
        Some(Hit::Face { body, index, .. }) => Some((body.clone(), *index, false)),
        Some(Hit::Edge { body, index, .. }) => Some((body.clone(), *index, true)),
        _ => None,
    };
    let Some(key) = key else {
        return if app.viewport.mouse.is_none() { LAST.with(|l| l.borrow().1.clone()) } else {
            LAST.with(|l| *l.borrow_mut() = (None, Vec::new()));
            Vec::new()
        };
    };
    if let Some(ids) = LAST.with(|l| { let l = l.borrow(); (l.0.as_ref() == Some(&key)).then(|| l.1.clone()) }) {
        return ids;
    }
    let world = crate::open_meshing::session_world(&app.session);
    let what = if key.2 {
        Picked::Edges
    } else {
        match world.body(&key.0).and_then(|b| kernel::guard("model hover", || Ok(kernel::face_surface(&b.body, key.1))).ok().flatten()) {
            Some(FaceSurface::Plane { .. }) => Picked::FlatFaces,
            Some(FaceSurface::Cylinder { .. }) => Picked::RoundFace,
            _ => Picked::OtherFaces,
        }
    };
    // The first few only: a ribbon lit all over says nothing.
    let ids: Vec<&'static str> =
        rows(what, 1).into_iter().filter_map(|r| if let Row::Start(id) = r { Some(id) } else { None }).take(4).collect();
    LAST.with(|l| *l.borrow_mut() = (Some(key), ids.clone()));
    ids
}

/// The tangent plane on the clicked round face that stands straight up (`vertical`) or lies level,
/// on the side of the face that was clicked: made with the point where it touches.
fn tangent_params(app: &SolveApp, vertical: bool) -> Result<serde_json::Value, String> {
    use solidcraft_engine::kernel::{self, FaceSurface};
    let Some(Sel::Face { body, index, point }) = app.session.selection.last().cloned() else {
        return Err("Click a round face first.".into());
    };
    let world = crate::open_meshing::session_world(&app.session);
    let Some(FaceSurface::Cylinder { origin, axis, radius }) =
        world.body(&body).and_then(|b| kernel::guard("model menu", || Ok(kernel::face_surface(&b.body, index))).ok().flatten())
    else {
        return Err("That face isn't round.".into());
    };
    let a = axis.vec();
    let d = point - origin;
    let on_axis = origin + a * d.dot(a);
    let clicked = (point - on_axis).normalized().ok_or("Click on the round face itself.")?;
    let up = solidcore_record::Vec3::new(0.0, 0.0, 1.0);
    // Vertical: the plane's normal level and square to the axis; level: the normal as near up or
    // down as the axis allows. Either on the side that was clicked.
    let mut n = if vertical { a.cross(up) } else { up - a * up.dot(a) };
    if n.len() < 1e-6 {
        // An upright cylinder: every tangent plane already stands up; none lies level.
        if vertical {
            n = clicked;
        } else {
            return Err("This round face stands upright: no tangent plane on it lies level.".into());
        }
    }
    let mut n = n.normalized().ok_or("No such plane on this face.")?;
    if n.dot(clicked) < 0.0 {
        n = -n;
    }
    let at = on_axis + n * radius;
    let name = if vertical { "Tangent (vertical)" } else { "Tangent (level)" };
    Ok(json!({"face": {"face": [at.x, at.y, at.z]}, "at": [at.x, at.y, at.z], "name": name}))
}

/// Draws the menu and handles the clicks that open it. Called once per frame.
pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    thread_local! {
        /// Open at this place for the selection as it was.
        static OPEN: std::cell::RefCell<Option<(egui::Pos2, Vec<Sel>)>> = const { std::cell::RefCell::new(None) };
        /// A click happened here: open once the selection has taken it.
        static PENDING: std::cell::Cell<Option<(egui::Pos2, u8)>> = const { std::cell::Cell::new(None) };
        static RECT: std::cell::Cell<Option<egui::Rect>> = const { std::cell::Cell::new(None) };
    }
    let idle = app.ui.tab == "MODEL"
        && app.tool.is_none()
        && app.dialog.is_none()
        && app.cam_editor.is_none()
        && app.session.active_sketch.is_none();
    if !idle {
        OPEN.with(|o| *o.borrow_mut() = None);
        PENDING.with(|p| p.set(None));
        RECT.with(|r| r.set(None));
        return;
    }
    let pointer = ctx.input(|i| i.pointer.interact_pos());
    let over_menu = pointer.is_some_and(|p| RECT.with(|r| r.get().is_some_and(|r| r.contains(p))));
    if ctx.input(|i| i.pointer.primary_clicked()) && app.viewport.mouse.is_some() && !over_menu
        && let Some(at) = pointer
    {
        PENDING.with(|p| p.set(Some((at + egui::vec2(14.0, 10.0), 3))));
    }
    // A few frames after the click the selection has it: open for it (or close on nothing).
    if let Some((at, left)) = PENDING.with(|p| p.get()) {
        if left > 0 {
            PENDING.with(|p| p.set(Some((at, left - 1))));
            ctx.request_repaint();
        } else {
            PENDING.with(|p| p.set(None));
            let sel = app.session.selection.clone();
            OPEN.with(|o| *o.borrow_mut() = if sel.is_empty() { None } else { Some((at, sel)) });
        }
    }
    let Some((at, sel)) = OPEN.with(|o| o.borrow().clone()) else { return };
    // The selection changed some other way (the Browser, Escape): the menu goes with it.
    if sel != app.session.selection || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        OPEN.with(|o| *o.borrow_mut() = None);
        RECT.with(|r| r.set(None));
        return;
    }
    let Some((what, count)) = picked(app) else { return };
    let rows = rows(what, count);
    if rows.is_empty() {
        return;
    }
    let t = Tokens::get();
    let mut chosen = None;
    let mut close = false;
    let shown = egui::Area::new(egui::Id::new("sc_model_click_menu")).order(egui::Order::Foreground).fixed_pos(at).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_min_width(190.0);
            let title = match what {
                Picked::Edges if count > 1 => format!("{count} edges:"),
                Picked::Edges => "This edge:".to_string(),
                Picked::FlatFaces | Picked::RoundFace | Picked::OtherFaces if count > 1 => format!("{count} faces:"),
                Picked::FlatFaces => "This flat face:".to_string(),
                Picked::RoundFace => "This round face:".to_string(),
                Picked::OtherFaces => "This face:".to_string(),
                Picked::Bodies if count > 1 => format!("{count} bodies:"),
                Picked::Bodies => "This body:".to_string(),
                Picked::MeshBody => "This mesh (STL):".to_string(),
                Picked::Profiles if count > 1 => format!("{count} profiles:"),
                Picked::Profiles => "This profile:".to_string(),
                Picked::Parts if count > 1 => format!("{count} parts:"),
                Picked::Parts => "This part:".to_string(),
            };
            ui.label(RichText::new(title).color(t.text_dim));
            for row in &rows {
                let label = match row {
                    Row::Tangent { vertical: true } => "Tangent Plane: Vertical".to_string(),
                    Row::Tangent { vertical: false } => "Tangent Plane: Level".to_string(),
                    Row::Start(id) => {
                        let own = solidcraft_engine::find_command(id).map(|c| c.label).unwrap_or(id);
                        solidcraft_ui_text::names::command(id).map_or_else(|| solidcraft_ui_text::tr_label(own).to_string(), str::to_string)
                    }
                };
                if ui.add(egui::Button::new(label).min_size(egui::vec2(180.0, 0.0))).clicked() {
                    chosen = Some(*row);
                }
            }
            ui.label(RichText::new("Shift-click adds more").color(t.text_dim).small());
            if ui.add(egui::Button::new(RichText::new("Close").color(t.text_dim)).frame(false)).clicked() {
                close = true;
            }
        });
    });
    RECT.with(|r| r.set(Some(shown.response.rect)));
    if close || chosen.is_some() {
        OPEN.with(|o| *o.borrow_mut() = None);
        RECT.with(|r| r.set(None));
    }
    match chosen {
        Some(Row::Start(id)) => app.start(id),
        Some(Row::Tangent { vertical }) => match tangent_params(app, vertical) {
            Ok(p) => match app.run("construct.plane.tangent", p) {
                Ok(_) => app.set_status(if vertical { "Vertical tangent plane made." } else { "Level tangent plane made." }.to_string(), false),
                Err(e) => app.set_status(e, true),
            },
            Err(e) => app.set_status(e, true),
        },
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::Harness;

    /// v20: on the Model tab a click on the model opens the menu of tools that fit, and a tool
    /// picked from it starts with what was clicked.
    #[test]
    fn a_click_on_the_model_lists_the_tools_that_fit_and_starts_them_on_it() {
        let mut h = Harness::new();
        h.app.run("solid.box", json!({"length": 40, "width": 30, "height": 10, "body_name": "Block"})).unwrap();
        h.app.run("solid.cylinder", json!({"diameter": 10, "height": 30, "base": [-20, 15, 5], "axis": "X", "body_name": "Rod"})).unwrap();
        h.app.ui.tab = "MODEL".into();
        h.settle();
        // A flat face: its tools.
        h.app.run("select.set", json!({"items": [{"type": "face", "body": "Block", "index": 0, "point": [20.0, 15.0, 10.0]}]})).unwrap();
        let (what, n) = picked(&h.app).expect("a face is picked");
        assert_eq!((what, n), (Picked::FlatFaces, 1));
        assert!(rows(what, n).contains(&Row::Start("sketch.create")));
        // The click opens the menu.
        h.app.run("select.clear", json!({})).unwrap();
        h.step(&json!({"move": {"world": [20.0, 15.0, 10.0]}})).unwrap();
        h.frames(4);
        h.step(&json!({"click": {"world": [20.0, 15.0, 10.0]}})).unwrap();
        h.frames(8);
        assert!(!h.app.session.selection.is_empty(), "the click selects");
        assert!(h.widget("This flat face:").is_some() || h.widget("This body:").is_some(), "the menu opens; status {:?}", h.app.status);
        // A round face along X: a vertical tangent plane made at once, standing up.
        let rod_face = {
            let world = crate::open_meshing::session_world(&h.app.session);
            let b = world.body("Rod").expect("the rod");
            (0..64).find(|i| {
                matches!(
                    solidcraft_engine::kernel::guard("t", || Ok(solidcraft_engine::kernel::face_surface(&b.body, *i))).ok().flatten(),
                    Some(solidcraft_engine::kernel::FaceSurface::Cylinder { .. })
                )
            })
        }
        .expect("the rod's round face");
        h.app.run("select.set", json!({"items": [{"type": "face", "body": "Rod", "index": rod_face, "point": [-10.0, 15.0, 10.0]}]})).unwrap();
        assert_eq!(picked(&h.app).map(|p| p.0), Some(Picked::RoundFace));
        let p = tangent_params(&h.app, true).unwrap();
        // Touching the side of the rod on the clicked side: a vertical plane through z = 5.
        let at = p["at"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
        assert!((at[2] - 5.0).abs() < 1e-6, "{p}");
        let level = tangent_params(&h.app, false).unwrap();
        let at = level["at"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
        assert!((at[2] - 10.0).abs() < 1e-6, "the top of the rod: {level}");
        h.app.run("construct.plane.tangent", p).unwrap();
        // An edge: Fillet starts with it.
        h.app.run("select.set", json!({"items": [{"type": "edge", "body": "Block", "index": 0, "point": [20.0, 0.0, 10.0]}]})).unwrap();
        assert_eq!(picked(&h.app).map(|p| p.0), Some(Picked::Edges));
        h.app.start("solid.fillet");
        h.frames(4);
        assert!(h.app.dialog.is_some() || h.app.tool.is_some(), "Fillet starts");
    }
}
