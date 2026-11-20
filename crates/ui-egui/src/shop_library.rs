//! The shop tool library window: the tools kept on this computer for every design. Import a
//! Fusion library (`.tools` / `.json`), a tool list or a spreadsheet; tick tools and add them to
//! the design; save the design's own tools into it. Picks and buttons only, no typing.

use std::collections::BTreeSet;

use egui::RichText;
use serde_json::{Value, json};

use solidcraft_theme::Tokens;

use crate::SolveApp;
use crate::ui_kit::Button;

/// The window's state between frames.
#[derive(Default)]
pub struct ShopWindow {
    /// What `cam.library.list` last returned.
    list: Value,
    /// Ticked tools, by their library index.
    checked: BTreeSet<usize>,
    /// The filters: an index into the kinds / libraries offered (0: all).
    kind: usize,
    library: usize,
    /// The last thing done, and whether it went wrong.
    message: Option<(String, bool)>,
}

/// Opens the window (or brings it back) with the library read fresh.
pub fn open(app: &mut SolveApp) {
    let mut w = app.shop_library.take().unwrap_or_default();
    refresh(app, &mut w);
    app.shop_library = Some(w);
}

fn refresh(app: &mut SolveApp, w: &mut ShopWindow) {
    match app.run("cam.library.list", json!({})) {
        Ok(v) => w.list = v,
        Err(e) => w.message = Some((e, true)),
    }
    let n = w.list["tools"].as_array().map_or(0, Vec::len);
    w.checked.retain(|i| *i < n);
}

/// A tool kind in words.
fn kind_word(kind: &str) -> &'static str {
    match kind {
        "flat_end" => "Flat end mill",
        "ball_end" => "Ball end mill",
        "bull_nose" => "Bull nose end mill",
        "chamfer" => "Chamfer / V-bit",
        "drill" => "Drill",
        "spot_drill" => "Spot drill",
        "tapered_ball" => "Tapered ball",
        "laser" => "Laser",
        "plasma" => "Plasma torch",
        _ => "Other",
    }
}

/// What a click asked for, done after the window is drawn.
enum Do {
    Import,
    SaveDesign,
    Add,
    Remove,
    All(bool),
}

pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut w) = app.shop_library.take() else { return };
    let t = Tokens::get();
    let tools: Vec<Value> = w.list["tools"].as_array().cloned().unwrap_or_default();
    // The filters offer only what the library holds.
    let mut kinds: Vec<&'static str> = Vec::new();
    let mut libraries: Vec<String> = Vec::new();
    for e in &tools {
        let k = kind_word(e["tool"]["kind"].as_str().unwrap_or(""));
        if !kinds.contains(&k) {
            kinds.push(k);
        }
        let l = e["library"].as_str().unwrap_or("").to_string();
        if !libraries.contains(&l) {
            libraries.push(l);
        }
    }
    kinds.sort_unstable();
    let kind_opts: Vec<&str> = std::iter::once("All kinds").chain(kinds.iter().copied()).collect();
    let lib_opts: Vec<String> = std::iter::once("All libraries".to_string()).chain(libraries.iter().cloned()).collect();
    w.kind = w.kind.min(kind_opts.len() - 1);
    w.library = w.library.min(lib_opts.len() - 1);
    let shown: Vec<usize> = tools
        .iter()
        .enumerate()
        .filter(|(_, e)| w.kind == 0 || kind_opts.get(w.kind).copied() == Some(kind_word(e["tool"]["kind"].as_str().unwrap_or(""))))
        .filter(|(_, e)| w.library == 0 || lib_opts.get(w.library).map(String::as_str) == e["library"].as_str())
        .map(|(i, _)| i)
        .collect();
    let mut todo: Option<Do> = None;
    let area = ctx.content_rect();
    let (closed, action) = crate::ui_kit::Window::new(egui::Id::new("sc_shop_library"), "ws_manufacture", "Tool Library")
        .cards(3.0)
        .placed(egui::Align2::CENTER_TOP, egui::pos2(area.center().x, area.top() + t.space_24 * 4.0))
        .actions("", "Close", None)
        .show(ctx, true, |ui| {
            ui.label(
                RichText::new("Tools kept on this computer for every design. Tick the ones you want and add them to this design.")
                    .color(t.text_dim),
            );
            ui.horizontal(|ui| {
                if crate::ui_kit::button(ui, "Import Library…", Button::Primary)
                    .on_hover_text("A Fusion tool library (.tools or .json), a tool list (.json) or a spreadsheet (.csv)")
                    .clicked()
                {
                    todo = Some(Do::Import);
                }
                if crate::ui_kit::button(ui, "Save This Design's Tools", Button::Normal)
                    .on_hover_text("Copy the tools of the open design into the library, as \"My tools\"")
                    .clicked()
                {
                    todo = Some(Do::SaveDesign);
                }
            });
            if tools.is_empty() {
                ui.add_space(t.space_8);
                ui.label("The library is empty: import a Fusion library or save this design's tools to start it.");
            } else {
                let lib_refs: Vec<&str> = lib_opts.iter().map(String::as_str).collect();
                crate::ui_kit::row(ui, "Kind", |ui| crate::ui_kit::dropdown(ui, "shop_kind", &mut w.kind, &kind_opts));
                crate::ui_kit::row(ui, "Library", |ui| crate::ui_kit::dropdown(ui, "shop_lib", &mut w.library, &lib_refs));
                ui.horizontal(|ui| {
                    if crate::ui_kit::button(ui, format!("Tick All {}", shown.len()), Button::Small).clicked() {
                        todo = Some(Do::All(true));
                    }
                    if crate::ui_kit::button(ui, "Untick All", Button::Small).clicked() {
                        todo = Some(Do::All(false));
                    }
                });
                egui::ScrollArea::vertical().max_height((ctx.content_rect().height() * 0.3).max(180.0)).show(ui, |ui| {
                    for &i in &shown {
                        let Some(e) = tools.get(i) else { continue };
                        let tl = &e["tool"];
                        let number = tl["number"].as_u64().unwrap_or(0);
                        let label = format!(
                            "{}{}  ·  {}  ·  {} mm",
                            if number > 0 { format!("T{number}  ") } else { String::new() },
                            tl["name"].as_str().unwrap_or(""),
                            kind_word(tl["kind"].as_str().unwrap_or("")),
                            (tl["diameter"].as_f64().unwrap_or(0.0) * 1000.0).round() / 1000.0,
                        );
                        let mut on = w.checked.contains(&i);
                        if crate::ui_kit::checkbox(ui, &label, &mut on).on_hover_text(e["library"].as_str().unwrap_or("")).changed() {
                            if on {
                                w.checked.insert(i);
                            } else {
                                w.checked.remove(&i);
                            }
                        }
                    }
                });
                ui.add_space(t.space_8);
                ui.horizontal(|ui| {
                    let n = w.checked.len();
                    if crate::ui_kit::button(ui, format!("Add {n} To This Design"), Button::Enabled(n > 0)).clicked() && n > 0 {
                        todo = Some(Do::Add);
                    }
                    if crate::ui_kit::button(ui, format!("Remove {n} From Library"), Button::Enabled(n > 0)).clicked() && n > 0 {
                        todo = Some(Do::Remove);
                    }
                });
            }
            if let Some((m, error)) = &w.message {
                ui.label(RichText::new(m).color(if *error { t.error } else { t.form_success }));
            }
        });
    match todo {
        Some(Do::Import) => {
            app.shop_library = Some(w);
            app.pick_open_for(&["tools", "json", "csv"], |app, path| {
                let Some(path) = path else { return };
                let result = app.run("cam.library.import", json!({"path": path}));
                let Some(mut w) = app.shop_library.take() else { return };
                w.message = Some(match result {
                    Ok(v) => {
                        let mut m = format!("{} tools added as \"{}\".", v["added"], v["library"].as_str().unwrap_or(""));
                        let found = v["found"].as_u64().unwrap_or(0);
                        let added = v["added"].as_u64().unwrap_or(0);
                        if found > added {
                            m.push_str(&format!(" {} were already there.", found - added));
                        }
                        if let Some(pl) = v["plain"].as_object().filter(|s| !s.is_empty()) {
                            let list: Vec<String> = pl.iter().map(|(k, n)| format!("{n} {}", k.replace('_', " "))).collect();
                            m.push_str(&format!(" Read as plain cutters of their diameter: {}.", list.join(", ")));
                        }
                        if let Some(sk) = v["skipped"].as_object().filter(|s| !s.is_empty()) {
                            let list: Vec<String> = sk.iter().map(|(k, n)| format!("{n} {k}")).collect();
                            m.push_str(&format!(" Left out (no diameter): {}.", list.join(", ")));
                        }
                        (m, false)
                    }
                    Err(e) => (e, true),
                });
                refresh(app, &mut w);
                app.shop_library = Some(w);
            });
            return;
        }
        Some(Do::SaveDesign) => {
            w.message = Some(match app.run("cam.library.save_design", json!({"library": "My tools"})) {
                Ok(v) => (format!("{} tools saved as \"My tools\".", v["added"]), false),
                Err(e) => (e, true),
            });
            refresh(app, &mut w);
        }
        Some(Do::Add) => {
            let picked: Vec<usize> = w.checked.iter().copied().collect();
            w.message = Some(match app.run("cam.library.add", json!({"indices": picked})) {
                Ok(v) => {
                    let n = v["tools"].as_array().map_or(0, Vec::len);
                    w.checked.clear();
                    (format!("{n} tools added to this design. They are in the Tools list and every operation's Tool box."), false)
                }
                Err(e) => (e, true),
            });
        }
        Some(Do::Remove) => {
            let picked: Vec<usize> = w.checked.iter().copied().collect();
            w.message = Some(match app.run("cam.library.remove", json!({"indices": picked})) {
                Ok(v) => (format!("{} tools removed from the library.", v["removed"]), false),
                Err(e) => (e, true),
            });
            w.checked.clear();
            refresh(app, &mut w);
        }
        Some(Do::All(on)) => {
            if on {
                w.checked.extend(shown.iter().copied());
            } else {
                w.checked.clear();
            }
        }
        None => {}
    }
    // The X and the Close button both close it.
    if !closed && !matches!(action, crate::ui_kit::Action::Cancel) {
        app.shop_library = Some(w);
    }
}
