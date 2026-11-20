//! The shop tool library: tools kept on the computer and shared by every design, read in from
//! Fusion tool libraries (`.tools` is a zip around the same `tools.json`), tool-list files
//! (this program's, HansFel's SolidCraftCam `tool-library.json`) and spreadsheets (`.csv`).
//!
//! Only sizes and shapes come in: no speeds or feeds, every shop sets its own.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tool::{Tool, ToolKind};

/// At most this many tools in the shop library.
pub const MAX_SHOP_TOOLS: usize = 20_000;

/// A tool in the shop library and the library it came from (a file's name, or a design's).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShopTool {
    pub library: String,
    pub tool: Tool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ShopLibrary {
    #[serde(default)]
    pub tools: Vec<ShopTool>,
}

/// What a file held: the tools read, by kind the ones read as plain cutters of their diameter
/// (no SolidCraft shape for them), and the ones left out (no diameter: lathe inserts, holders).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Imported {
    pub tools: Vec<Tool>,
    pub plain: Vec<(String, usize)>,
    pub skipped: Vec<(String, usize)>,
}

fn count(list: &mut Vec<(String, usize)>, kind: &str) {
    match list.iter_mut().find(|(k, _)| k == kind) {
        Some((_, n)) => *n += 1,
        None => list.push((kind.to_string(), 1)),
    }
}

impl Imported {
    fn skip(&mut self, kind: &str) {
        count(&mut self.skipped, kind);
    }
    fn plain(&mut self, kind: &str) {
        count(&mut self.plain, kind);
    }
}

/// Whether a tool of `kind` read from a file of type `ty` is a stand-in (a flat cutter for a
/// type that is not one).
fn plain_kind(kind: &ToolKind, ty: &str) -> bool {
    *kind == ToolKind::FlatEnd && !matches!(ty, "flat end mill" | "face mill" | "bull nose end mill")
}

impl ShopLibrary {
    /// Adds `tools` under `library`; a tool the library already has there (same name, kind and
    /// size) is left out. Returns how many were added.
    pub fn add(&mut self, library: &str, tools: Vec<Tool>) -> usize {
        let mut added = 0;
        for tool in tools {
            if self.tools.len() >= MAX_SHOP_TOOLS {
                break;
            }
            let same = |t: &ShopTool| {
                t.library == library && t.tool.name == tool.name && t.tool.kind == tool.kind && (t.tool.diameter - tool.diameter).abs() < 1e-6
            };
            if self.tools.iter().any(same) {
                continue;
            }
            self.tools.push(ShopTool { library: library.to_string(), tool });
            added += 1;
        }
        added
    }
}

/// Reads a tool file's text (a `.tools` file is unzipped by the caller): a Fusion library
/// (`{"data": [...]}`), a tool list (`{"tools": [...]}` or `[...]`) or a spreadsheet.
pub fn read(text: &str) -> Result<Imported, String> {
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.starts_with('{') || text.starts_with('[') {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("not a tool library file: {e}"))?;
        if let Some(data) = v.get("data").and_then(Value::as_array) {
            return Ok(fusion(data));
        }
        if let Some(list) = v.get("tools").and_then(Value::as_array).or_else(|| v.as_array()) {
            return Ok(tool_list(list));
        }
        return Err("this JSON file holds no tools (no `data` or `tools` list)".into());
    }
    csv(text)
}

fn f(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite())
}

/// A tool from its sizes (mm), with the lengths filled in where a file leaves them out.
fn make(name: &str, number: u32, kind: ToolKind, diameter: f64, flute: Option<f64>, overall: Option<f64>, shank: Option<f64>, flutes: Option<u32>) -> Option<Tool> {
    if !(diameter.is_finite() && diameter > 0.0 && diameter <= 1000.0) {
        return None;
    }
    let mut t = Tool::new(1, name, kind, diameter);
    t.number = number;
    t.flute_length = flute.filter(|x| *x > 0.0).unwrap_or(diameter * 3.0);
    t.overall_length = overall.filter(|x| *x >= t.flute_length).unwrap_or((t.flute_length * 2.0).max(t.flute_length + 10.0));
    t.shank_diameter = shank.filter(|x| *x > 0.0).unwrap_or(diameter);
    if let Some(n) = flutes.filter(|n| *n > 0 && *n <= 100) {
        t.flutes = n;
    }
    t.validate().ok()?;
    Some(t)
}

/// A Fusion tool library's `data` list.
fn fusion(data: &[Value]) -> Imported {
    let mut out = Imported::default();
    for t in data {
        let ty = t.get("type").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
        if ty == "holder" || ty == "probe" || ty.is_empty() {
            continue;
        }
        let g = t.get("geometry").cloned().unwrap_or(Value::Null);
        let unit = if t.get("unit").and_then(Value::as_str).is_some_and(|u| u.starts_with("inch")) { 25.4 } else { 1.0 };
        let mm = |k: &str| f(&g, k).map(|x| x * unit);
        // A torch or a beam: its kerf width (CW) is its size; the plasma cuts at the head clearance.
        if ty == "waterjet" {
            if let Some(kerf) = mm("CW") {
                out.plain(&ty);
                if let Some(tool) = make(&format!("Waterjet {} mm kerf", (kerf * 1000.0).round() / 1000.0), 0, ToolKind::FlatEnd, kerf, None, None, None, None) {
                    out.tools.push(tool);
                }
                continue;
            }
        }
        if ty == "plasma cutter" || ty == "laser cutter" {
            let name = t.get("description").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
            let (kind, label) = if ty == "plasma cutter" {
                let cut = mm("JET_HEAD_CLEARANCE").filter(|c| *c > 0.0 && *c <= 50.0).unwrap_or(1.5);
                (ToolKind::Plasma { pierce_height: (cut * 2.5).clamp(cut, 50.0), cut_height: cut, pierce_delay_s: 0.5 }, "Plasma torch")
            } else {
                (ToolKind::Laser { wavelength_nm: 445.0 }, "Laser")
            };
            let Some(kerf) = mm("CW") else {
                out.skip(&ty);
                continue;
            };
            let name = name.unwrap_or_else(|| format!("{label} {} mm kerf", (kerf * 1000.0).round() / 1000.0));
            match make(&name, 0, kind, kerf, None, None, None, None) {
                Some(tool) => out.tools.push(tool),
                None => out.skip(&ty),
            }
            continue;
        }
        let Some(dc) = mm("DC") else {
            out.skip(&ty);
            continue;
        };
        let shank = mm("SFDM");
        let re = mm("RE").unwrap_or(0.0);
        let ta = f(&g, "TA");
        let sig = f(&g, "SIG");
        let (kind, diameter) = match ty.as_str() {
            "flat end mill" | "face mill" => (ToolKind::FlatEnd, dc),
            "ball end mill" => (ToolKind::BallEnd, dc),
            "bull nose end mill" if re > 0.0 && re < dc * 0.5 => (ToolKind::BullNose { corner_radius: re }, dc),
            "bull nose end mill" if re >= dc * 0.5 => (ToolKind::BallEnd, dc),
            "bull nose end mill" => (ToolKind::FlatEnd, dc),
            "chamfer mill" => (ToolKind::Chamfer { tip_angle_deg: 2.0 * ta.unwrap_or(45.0) }, dc),
            "counter sink" => (ToolKind::Chamfer { tip_angle_deg: sig.unwrap_or(90.0) }, dc),
            "spot drill" | "center drill" => (ToolKind::SpotDrill { tip_angle_deg: sig.unwrap_or(90.0) }, dc),
            "drill" => (ToolKind::Drill { tip_angle_deg: sig.unwrap_or(118.0) }, dc),
            // A V-bit (a wide taper, a sharp tip) cuts like a chamfer mill; a carving bit is a
            // tapered ball whose ball is the tip and whose widest is the shank.
            "tapered mill" if ta.unwrap_or(0.0) >= 15.0 && re <= 1e-6 => (ToolKind::Chamfer { tip_angle_deg: 2.0 * ta.unwrap_or(45.0) }, dc),
            "tapered mill" => {
                let widest = shank.unwrap_or(dc).max(dc);
                (ToolKind::TaperedBall { tip_radius: if re > 0.0 { re } else { dc * 0.5 }.clamp(0.001, widest * 0.5), taper_angle_deg: ta.unwrap_or(1.0) }, widest)
            }
            // Anything else (taps, reamers, T-slots, thread mills…): a plain cutter of its
            // diameter, which is all a post needs; its own type stays in the name.
            _ => {
                out.plain(&ty);
                (ToolKind::FlatEnd, dc)
            }
        };
        let described = t.get("description").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        let name = match (described, plain_kind(&kind, &ty)) {
            (Some(d), true) if !d.to_ascii_lowercase().contains(&ty) => format!("{d} ({ty})"),
            (Some(d), _) => d.to_string(),
            (None, _) => format!("{ty} {} mm", (dc * 1000.0).round() / 1000.0),
        };
        let number = t.pointer("/post-process/number").and_then(Value::as_u64).map_or(0, |n| n.min(9999) as u32);
        let flutes = g.get("NOF").and_then(Value::as_u64).map(|n| n as u32);
        match make(&name, number, kind, diameter, mm("LCF"), mm("OAL"), shank, flutes) {
            Some(tool) => out.tools.push(tool),
            None => out.skip(&ty),
        }
    }
    out
}

/// A tool kind from a word: this program's (`flat_end`), HansFel's (`flat_endmill`, `v_bit`) or
/// a person's (`flat`, `end mill`, `v-bit`).
fn kind_of(word: &str, corner: Option<f64>, tip: Option<f64>, diameter: f64) -> Option<ToolKind> {
    let w: String = word.to_ascii_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    Some(match w.as_str() {
        "flat" | "flatend" | "flatendmill" | "endmill" | "square" | "facemill" => ToolKind::FlatEnd,
        "ball" | "ballend" | "ballendmill" | "ballnose" => ToolKind::BallEnd,
        "bull" | "bullnose" | "bullnoseendmill" | "radius" => match corner {
            Some(r) if r > 0.0 && r < diameter * 0.5 => ToolKind::BullNose { corner_radius: r },
            _ => ToolKind::FlatEnd,
        },
        "chamfer" | "chamfermill" | "countersink" => ToolKind::Chamfer { tip_angle_deg: tip.unwrap_or(90.0) },
        "vbit" | "vgroove" | "engrave" | "engraver" => ToolKind::Chamfer { tip_angle_deg: tip.unwrap_or(60.0) },
        "drill" | "twistdrill" => ToolKind::Drill { tip_angle_deg: tip.unwrap_or(118.0) },
        "spot" | "spotdrill" | "centredrill" | "centerdrill" => ToolKind::SpotDrill { tip_angle_deg: tip.unwrap_or(90.0) },
        "plasma" | "torch" | "plasmatorch" => ToolKind::Plasma { pierce_height: 3.8, cut_height: 1.5, pierce_delay_s: 0.5 },
        _ => return None,
    })
}

/// A tool list: this program's tools as saved, or another program's with the usual names.
fn tool_list(items: &[Value]) -> Imported {
    let mut out = Imported::default();
    for v in items {
        if let Ok(mut t) = serde_json::from_value::<Tool>(v.clone())
            && t.validate().is_ok()
        {
            t.id = 1;
            out.tools.push(t);
            continue;
        }
        let word = v.get("kind").or_else(|| v.get("type")).and_then(Value::as_str).unwrap_or("");
        let Some(diameter) = f(v, "diameter") else {
            out.skip(if word.is_empty() { "unknown" } else { word });
            continue;
        };
        // A kind with no SolidCraft shape: a plain cutter of its diameter, its kind in the name.
        let (mut kind, plain) = match kind_of(word, f(v, "corner_radius"), f(v, "tip_angle").or_else(|| f(v, "tip_angle_deg")), diameter) {
            Some(k) => (k, false),
            None => {
                out.plain(if word.is_empty() { "unknown" } else { word });
                (ToolKind::FlatEnd, true)
            }
        };
        if let ToolKind::Plasma { pierce_height, cut_height, pierce_delay_s } = &mut kind {
            *pierce_height = f(v, "pierce_height").unwrap_or(*pierce_height);
            *cut_height = f(v, "cut_height").unwrap_or(*cut_height);
            *pierce_delay_s = f(v, "pierce_delay").or_else(|| f(v, "pierce_delay_s")).unwrap_or(*pierce_delay_s);
        }
        let name = ["name", "description"]
            .iter()
            .find_map(|k| v.get(*k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()))
            .map(str::to_string)
            .unwrap_or_else(|| format!("{word} {diameter} mm"));
        let label = word.replace('_', " ");
        let name = if plain && !name.to_ascii_lowercase().contains(&label.to_ascii_lowercase()) { format!("{name} ({label})") } else { name };
        let number = v.get("number").and_then(Value::as_u64).map_or(0, |n| n.min(9999) as u32);
        let flutes = v.get("flutes").and_then(Value::as_u64).map(|n| n as u32);
        let overall = f(v, "overall_length").or_else(|| f(v, "length"));
        match make(&name, number, kind, diameter, f(v, "flute_length"), overall, f(v, "shank_diameter"), flutes) {
            Some(t) => out.tools.push(t),
            None => out.skip(word),
        }
    }
    out
}

/// A spreadsheet saved as CSV, one tool a row under a header row naming the columns: name,
/// kind, diameter (mm), and any of number, flute_length, overall_length, shank_diameter,
/// flutes, corner_radius, tip_angle.
fn csv(text: &str) -> Result<Imported, String> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header = lines.next().ok_or("the file is empty")?;
    let sep = if header.matches(';').count() > header.matches(',').count() { ';' } else { ',' };
    let cells = |l: &str| -> Vec<String> { l.split(sep).map(|c| c.trim().trim_matches('"').trim().to_string()).collect() };
    let names: Vec<String> = cells(header)
        .iter()
        .map(|h| h.to_ascii_lowercase().replace(['(', ')'], "").replace("mm", "").trim().replace([' ', '-'], "_"))
        .collect();
    let col = |want: &[&str]| names.iter().position(|n| want.contains(&n.as_str()));
    let Some(dia) = col(&["diameter", "dia", "d", "dc"]) else {
        return Err("not a tool file: a spreadsheet needs a header row with a `diameter` column".into());
    };
    let kind_col = col(&["kind", "type", "tool_type"]);
    let name_col = col(&["name", "description"]);
    let mut list = Vec::new();
    for l in lines {
        let c = cells(l);
        let get = |i: Option<usize>| i.and_then(|i| c.get(i)).map(String::as_str).unwrap_or("");
        let mut o = serde_json::Map::new();
        o.insert("kind".into(), Value::from(if get(kind_col).is_empty() { "flat" } else { get(kind_col) }));
        if !get(name_col).is_empty() {
            o.insert("name".into(), Value::from(get(name_col)));
        }
        for (key, want) in [
            ("diameter", &["diameter", "dia", "d", "dc"][..]),
            ("number", &["number", "t", "tool", "tool_number"][..]),
            ("flute_length", &["flute_length", "flute", "lcf", "cutting_length"][..]),
            ("overall_length", &["overall_length", "length", "oal", "stick_out", "stickout"][..]),
            ("shank_diameter", &["shank_diameter", "shank", "sfdm"][..]),
            ("flutes", &["flutes", "nof"][..]),
            ("corner_radius", &["corner_radius", "radius", "re"][..]),
            ("tip_angle", &["tip_angle", "angle", "tip_angle_deg"][..]),
        ] {
            let i = if key == "diameter" { Some(dia) } else { col(want) };
            if let Ok(x) = get(i).replace(',', ".").parse::<f64>() {
                let n = serde_json::Number::from_f64(x).map(Value::Number).unwrap_or(Value::Null);
                let n = if matches!(key, "number" | "flutes") { Value::from(x.max(0.0).round() as u64) } else { n };
                o.insert(key.into(), n);
            }
        }
        list.push(Value::Object(o));
    }
    Ok(tool_list(&list))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fusion_library_reads_shapes_in_mm_and_skips_the_rest() {
        let text = r#"{"data": [
            {"type": "flat end mill", "unit": "inches", "description": "1/4 flat", "geometry": {"DC": 0.25, "LCF": 0.75, "OAL": 2.5, "SFDM": 0.25, "NOF": 2}, "post-process": {"number": 4}},
            {"type": "bull nose end mill", "unit": "millimeters", "description": "6 bull", "geometry": {"DC": 6, "RE": 1, "LCF": 12, "OAL": 60}},
            {"type": "tapered mill", "unit": "inches", "description": "V 60", "geometry": {"DC": 0.25, "TA": 30, "LCF": 0.25, "OAL": 2.5, "tip-diameter": 0}},
            {"type": "tapered mill", "unit": "inches", "description": "carving", "geometry": {"DC": 0.0625, "RE": 0.03125, "TA": 1, "SFDM": 0.25, "LCF": 1, "OAL": 3}},
            {"type": "drill", "unit": "millimeters", "description": "4 drill", "geometry": {"DC": 4, "SIG": 140, "LCF": 20, "OAL": 55}},
            {"type": "thread mill", "unit": "millimeters", "description": "TM", "geometry": {"DC": 6}},
            {"type": "holder", "description": "ER32"}
        ]}"#;
        let r = read(text).unwrap();
        assert_eq!(r.tools.len(), 6, "{r:?}");
        let flat = &r.tools[0];
        assert!((flat.diameter - 6.35).abs() < 1e-9 && (flat.flute_length - 19.05).abs() < 1e-9 && flat.number == 4);
        assert_eq!(r.tools[1].kind, ToolKind::BullNose { corner_radius: 1.0 });
        assert!(matches!(r.tools[2].kind, ToolKind::Chamfer { tip_angle_deg } if (tip_angle_deg - 60.0).abs() < 1e-9));
        assert!(matches!(r.tools[3].kind, ToolKind::TaperedBall { tip_radius, .. } if (tip_radius - 0.79375).abs() < 1e-9));
        assert!((r.tools[3].diameter - 6.35).abs() < 1e-9, "a carving bit is as wide as its shank");
        assert_eq!(r.tools[4].kind, ToolKind::Drill { tip_angle_deg: 140.0 });
        // No shape for a thread mill: a plain cutter of its diameter, named for what it is.
        assert_eq!(r.tools[5].kind, ToolKind::FlatEnd);
        assert_eq!(r.tools[5].name, "TM (thread mill)");
        assert_eq!(r.plain, vec![("thread mill".to_string(), 1)]);
        assert!(r.skipped.is_empty(), "the holder is no tool: {r:?}");
    }

    /// Local check against real libraries: `SHOP_REAL_DIR` holds folders of `tools.json`.
    #[test]
    fn real_libraries_when_given() {
        let Some(dir) = std::env::var_os("SHOP_REAL_DIR") else { return };
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let Ok(text) = std::fs::read_to_string(e.path().join("tools.json")) else { continue };
            match read(&text) {
                Ok(r) => println!("REAL {:?}: {} tools, skipped {:?}", e.file_name(), r.tools.len(), r.skipped),
                Err(err) => println!("REAL {:?}: ERROR {err}", e.file_name()),
            }
        }
    }

    #[test]
    fn tool_lists_and_spreadsheets_read() {
        // HansFel's SolidCraftCam export.
        let hans = r#"{"tools": [
            {"number": 3, "kind": "flat_endmill", "diameter": 6, "flute_length": 20, "description": "6 mm flat"},
            {"number": 7, "kind": "v_bit", "diameter": 12, "flute_length": 5, "tip_angle": 90, "description": "V90"},
            {"number": 9, "kind": "tap", "diameter": 5, "flute_length": 10}
        ]}"#;
        let r = read(hans).unwrap();
        assert_eq!(r.tools.len(), 3, "{r:?}");
        assert_eq!(r.tools[1].kind, ToolKind::Chamfer { tip_angle_deg: 90.0 });
        assert_eq!(r.tools[0].number, 3);
        assert_eq!((r.tools[2].kind, r.tools[2].number), (ToolKind::FlatEnd, 9));
        assert_eq!(r.plain, vec![("tap".to_string(), 1)]);
        // This program's own tools come back as they were.
        let mine = Tool::new(5, "3 mm ball", ToolKind::BallEnd, 3.0);
        let r = read(&serde_json::json!({"tools": [mine]}).to_string()).unwrap();
        assert_eq!(r.tools.len(), 1);
        assert_eq!(r.tools[0].kind, ToolKind::BallEnd);
        // A spreadsheet.
        let sheet = "Name,Kind,Diameter (mm),T,Flute length,Length\n1/8 flat,flat,3.175,5,12,38\n4 mm drill,drill,4,22,20,55\n";
        let r = read(sheet).unwrap();
        assert_eq!(r.tools.len(), 2, "{r:?}");
        assert_eq!(r.tools[0].number, 5);
        assert!(matches!(r.tools[1].kind, ToolKind::Drill { .. }));
        // The library keeps one of each.
        let mut lib = ShopLibrary::default();
        assert_eq!(lib.add("Shop", r.tools.clone()), 2);
        assert_eq!(lib.add("Shop", r.tools), 0);
    }
}
