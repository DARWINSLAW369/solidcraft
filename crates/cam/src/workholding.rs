//! How to hold the part (`cam.workholding`, docs/cam-ai-planning.md step 3, "workholding
//! first"). How a part is held decides the setups, the stock, what is finished by hand and how
//! far the part bends under the cut, so the planner compares the ways it could be held before
//! it plans operations, instead of taking one for granted.
//!
//! Each strategy is checked against the part's geometry and the machine, then costed on the
//! same terms: spots finished by hand, setups, extra material cut only to hold the part, how far
//! the part bends at its far end under a finishing cut, how well a turned-over part is located,
//! and the fixture to make. The costs are numbers the assistant can weigh, each with a reason.
//!
//! Everything is measured in the setup's frame (its WCS): +z is the tool axis of the main setup.

use serde::Serialize;
use serde_json::{Map, Value, json};
use solidcore_record::{Vec2, Vec3};
use solidcraft_geom::prelude::*;

use crate::analyze::{Analysis, BodyInput, Surface};
use crate::dropcutter::DropMesh;
use crate::frame::Frame;
use crate::machine::{Kinematics, Machine};
use crate::notice::{Issue, Notice, r3};
use crate::tool::{Tool, ToolKind};
use crate::{CamError, Result};

/// What the comparison needs beyond the part and the machine.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// The largest roughing tool's diameter (mm): the ring cut round a part held in a frame.
    pub tool_diameter: f64,
    /// The material's Young's modulus (N/mm²): how stiff the part is.
    pub modulus: f64,
    /// The side force of a finishing cut (N), pushing the far end of a part held at one end.
    pub finishing_force: f64,
    /// Deflection at the far end (mm) above which a part held at one end is a risk.
    pub deflection_limit: f64,
    /// The stock's box in the same frame, when known.
    pub stock: Option<(Vec3, Vec3)>,
}

impl Default for Options {
    fn default() -> Options {
        Options { tool_diameter: 6.0, modulus: 69_000.0, finishing_force: 10.0, deflection_limit: 0.02, stock: None }
    }
}

/// One step of a strategy: a setup, how the part is held in it and what it cuts.
#[derive(Clone, Debug, Serialize)]
pub struct Step {
    pub name: String,
    /// The tool axis in the frame (towards the spindle), as `+z`, `-y`…
    pub direction: String,
    pub held_by: String,
    pub cuts: String,
}

/// A body to add as a fixture, in the frame: a box or a cylinder.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum Fixture {
    Box { name: String, min: [f64; 3], max: [f64; 3], setups: Vec<String> },
    Cylinder { name: String, base: [f64; 3], axis: [f64; 3], radius: f64, length: f64, setups: Vec<String> },
}

/// One way to hold the part, checked and costed.
#[derive(Clone, Debug, Serialize)]
pub struct Strategy {
    /// `tabs`, `sprues`, `vise`, `pin`, `stub`, `rotary`.
    pub id: &'static str,
    pub name: &'static str,
    pub feasible: bool,
    /// The sum of the costs; lower is better. Infeasible strategies have none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// One sentence: why it suits this part or why it does not.
    pub why: String,
    /// Each cost with its points: `hand_finish`, `setups`, `extra_cut`, `deflection`, `location`,
    /// `fixture`.
    pub costs: Map<String, Value>,
    /// The numbers behind the costs.
    pub values: Map<String, Value>,
    pub steps: Vec<Step>,
    /// Spots finished by hand after the machine.
    pub hand_finish: Vec<String>,
    pub fixtures: Vec<Fixture>,
    pub issues: Vec<Issue>,
}

/// The comparison: every strategy, best first, and the one recommended.
#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub strategies: Vec<Strategy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended: Option<&'static str>,
    pub notice: Notice,
}

/// Points per cost: what one unit of each is worth against the others.
const PER_HAND_SPOT: f64 = 6.0;
const PER_SETUP: f64 = 2.0;
/// Per cm³ cut only to hold the part.
const PER_EXTRA_CM3: f64 = 0.5;
/// Per deflection limit reached at the far end.
const PER_DEFLECTION_LIMIT: f64 = 6.0;
const DEFLECTION_CAP: f64 = 40.0;
/// A turned-over part found again from the stock's box, not a feature of the part.
const LOOSE_LOCATION: f64 = 3.0;
/// A pin fixture or soft jaws to make.
const FIXTURE_TO_MAKE: f64 = 3.0;
/// The side force of a roughing cut (N) a pinned part must not turn under, with a margin.
const ROUGHING_FORCE: f64 = 60.0;
const TORQUE_MARGIN: f64 = 1.5;
/// A pin's bolt: preload as 60 % of an 8.8 bolt's proof stress on its stress area, and the
/// friction of the clamped faces.
const BOLT_PROOF_MPA: f64 = 640.0;
const CLAMP_FRICTION: f64 = 0.15;
/// The post that carries the pin: its thickness along the pin and the room below the part.
const POST_THICKNESS: f64 = 8.0;
const POST_BELOW: f64 = 10.0;

/// The outline's sectors when looking for a flat waist.
const SECTORS: usize = 72;
/// The least grip (mm) a vise jaw or clamp needs on a face, in both directions.
const MIN_GRIP: f64 = 3.0;
/// A sacrificial stub's grip length and section (mm).
const STUB_GRIP: f64 = 12.0;
const STUB_SECTION: f64 = 10.0;
/// Stations along a part held at one end, and samples across each.
const STATIONS: usize = 40;
const ACROSS: usize = 48;

/// A cylindrical face found on the part.
#[derive(Clone, Copy, Debug)]
struct Cyl {
    origin: Vec3,
    axis: Vec3,
    radius: f64,
    /// Its extent along the axis, from `origin`.
    a0: f64,
    a1: f64,
    concave: bool,
    full: bool,
}

/// A planar face found on the part.
#[derive(Clone, Copy, Debug)]
struct Flat {
    normal: Vec3,
    /// A point on it.
    at: Vec3,
    area: f64,
    face: usize,
    body: usize,
}

/// Compares the ways to hold the part: `bodies` in the setup's frame, `analysis` from
/// [`crate::analyze::analyze`] on the same bodies and frame.
pub fn plan(bodies: &[BodyInput], analysis: &Analysis, machine: &Machine, opts: &Options) -> Result<Plan> {
    let points: Vec<Vec3> = bodies.iter().flat_map(|b| b.mesh.positions.iter().copied()).filter(|p| p.is_finite()).collect();
    if points.is_empty() {
        return Err(CamError::Geometry("there is no body to hold".into()));
    }
    let (lo, hi) = bounds(&points);
    let size = hi - lo;
    let drop = DropMesh::from_meshes(bodies.iter().map(|b| b.mesh));
    let (cyls, flats) = surfaces(bodies);
    let tool_d = if opts.tool_diameter.is_finite() && opts.tool_diameter > 0.0 { opts.tool_diameter.min(50.0) } else { 6.0 };
    let modulus = if opts.modulus.is_finite() && opts.modulus > 0.0 { opts.modulus } else { 69_000.0 };
    let force = if opts.finishing_force.is_finite() && opts.finishing_force > 0.0 { opts.finishing_force.min(1000.0) } else { 10.0 };
    let limit = if opts.deflection_limit.is_finite() && opts.deflection_limit > 0.0 { opts.deflection_limit } else { 0.02 };
    let beam = Beam { drop: &drop, modulus, force, limit, lo, hi };
    let height = opts.stock.map_or(size.z, |(a, b)| (b.z - a.z).max(size.z));

    // The outline, seen along the tool axis: its farthest point in each sector round the
    // middle, and the height of the part's edge there.
    let mid = (lo + hi) * 0.5;
    let mut sector: Vec<Option<(f64, Vec3)>> = vec![None; SECTORS];
    for p in &points {
        let (dx, dy) = (p.x - mid.x, p.y - mid.y);
        let r = dx.hypot(dy);
        let k = (((dy.atan2(dx) + std::f64::consts::PI) / std::f64::consts::TAU * SECTORS as f64) as usize).min(SECTORS - 1);
        if let Some(slot) = sector.get_mut(k)
            && slot.is_none_or(|(best, _)| r > best)
        {
            *slot = Some((r, *p));
        }
    }
    let rim: Vec<Vec3> = sector.iter().flatten().map(|(_, p)| *p).collect();
    let perimeter: f64 = rim.iter().zip(rim.iter().cycle().skip(1)).map(|(a, b)| (a.x - b.x).hypot(a.y - b.y)).sum();
    let mut edge_z: Vec<f64> = rim.iter().map(|p| p.z).collect();
    edge_z.sort_by(f64::total_cmp);
    let pct = |q: f64| edge_z.get(((edge_z.len().saturating_sub(1)) as f64 * q).round() as usize).copied().unwrap_or(0.0);
    let waist_spread = pct(0.9) - pct(0.1);
    // A frame round the part: the ring cut to free it, the tool's width and a millimetre more.
    let ring_cm3 = perimeter * (tool_d + 1.0) * height / 1000.0;
    let sides = analysis.directions.len().max(2);

    let mut out: Vec<Strategy> = Vec::new();

    // 1. A frame with a web and tabs: needs a waist, where the part's edge is at one height.
    {
        let flat_waist = waist_spread <= (0.05 * size.z).max(1.0);
        let tabs = ((perimeter / 40.0).ceil() as usize).clamp(3, 8);
        let mut s = strategy("tabs", "Frame, web and tabs");
        s.values.insert("edge_height_spread_mm".into(), json!(r3(waist_spread)));
        s.values.insert("perimeter_mm".into(), json!(r3(perimeter)));
        s.values.insert("tabs".into(), json!(tabs));
        if flat_waist {
            s.feasible = true;
            s.why = format!("the part's edge stays within {:.1} mm of one height, so a web left at that height meets it all round", waist_spread);
            cost(&mut s, "hand_finish", tabs as f64 * PER_HAND_SPOT, format!("{tabs} tabs cut off and filed"));
            cost(&mut s, "setups", sides as f64 * PER_SETUP, format!("{sides} setups"));
            cost(&mut s, "extra_cut", ring_cm3 * PER_EXTRA_CM3, format!("{ring_cm3:.1} cm³ ring round the part, then the web"));
            cost(&mut s, "location", LOOSE_LOCATION, "the turned-over stock is found again from its box".into());
            s.hand_finish = vec![format!("{tabs} tabs round the edge")];
            s.steps = frame_steps(&analysis.directions, "the stock's frame in a vise");
        } else {
            s.why = format!(
                "the part's edge rises and falls {:.1} mm round its outline, so no web height meets it all round: tabs would hold nothing",
                waist_spread
            );
            s.issues.push(
                Issue::info("no_waist", s.why.clone())
                    .limit(json!(r3((0.05 * size.z).max(1.0))))
                    .parameter("edge_height_spread_mm", r3(waist_spread)),
            );
        }
        out.push(s);
    }

    // 2. A frame with sprues: works for any outline.
    {
        let n = 3usize;
        let mut s = strategy("sprues", "Frame and sprues");
        s.feasible = true;
        s.why = "small bars from the frame to the part where its edge is thickest hold any outline, whatever its edge heights".into();
        s.values.insert("sprues".into(), json!(n));
        s.values.insert("perimeter_mm".into(), json!(r3(perimeter)));
        cost(&mut s, "hand_finish", n as f64 * PER_HAND_SPOT * 1.25, format!("{n} sprues sawn off and filed flush with a free-form edge"));
        cost(&mut s, "setups", sides as f64 * PER_SETUP, format!("{sides} setups"));
        cost(&mut s, "extra_cut", ring_cm3 * PER_EXTRA_CM3, format!("{ring_cm3:.1} cm³ ring round the part, then the cut under the sprues"));
        cost(&mut s, "location", LOOSE_LOCATION, "the turned-over stock is found again from its box".into());
        s.hand_finish = vec![format!("{n} sprue stubs on the edge")];
        s.steps = frame_steps(&analysis.directions, "the stock's frame in a vise");
        out.push(s);
    }

    // 3. A pin through a hole of the part, its flat faces clamped: the part's own feature holds it.
    let pin = pin_strategy(&cyls, &flats, &beam, &points, analysis, tool_d);
    out.push(pin);

    // 4. A vise on two parallel faces of the part.
    out.push(vise_strategy(&flats, bodies, &cyls, analysis));

    // 5. A sacrificial stub at one end, gripped in a vise.
    let stub = stub_end(&beam, lo, hi);
    out.push(stub_strategy(&stub, &beam, analysis));

    // 6. The same stub in a rotary module's chuck.
    out.push(rotary_strategy(&stub, &beam, machine, &points));

    // Best first: feasible by score, then the rest.
    for s in &mut out {
        if s.feasible {
            let total: f64 = s.costs.values().filter_map(|v| v.get("points").and_then(Value::as_f64)).sum();
            s.score = Some(r3(total));
        }
    }
    out.sort_by(|a, b| match (a.score, b.score) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let recommended = out.first().filter(|s| s.feasible).map(|s| s.id);
    let feasible = out.iter().filter(|s| s.feasible).count();
    let mut notice = Notice::new(match recommended {
        Some(id) => format!("{feasible} of {} ways to hold the part work; best: {id}", out.len()),
        None => "no way to hold the part was found".into(),
    });
    notice.value("feasible", feasible);
    notice.value("part_size_mm", json!([r3(size.x), r3(size.y), r3(size.z)]));
    if let (Some(a), Some(b)) = (out.first(), out.get(1))
        && let (Some(x), Some(y)) = (a.score, b.score)
        && (y - x).abs() < 3.0
    {
        notice.issue(Issue::info(
            "close_call",
            format!("`{}` and `{}` score within 3 points: the person's fixtures and priorities decide", a.id, b.id),
        ));
    }
    Ok(Plan { strategies: out, recommended, notice })
}

fn strategy(id: &'static str, name: &'static str) -> Strategy {
    Strategy {
        id,
        name,
        feasible: false,
        score: None,
        why: String::new(),
        costs: Map::new(),
        values: Map::new(),
        steps: Vec::new(),
        hand_finish: Vec::new(),
        fixtures: Vec::new(),
        issues: Vec::new(),
    }
}

fn cost(s: &mut Strategy, key: &str, points: f64, why: String) {
    s.costs.insert(key.into(), json!({"points": r3(points.max(0.0)), "why": why}));
}

fn bounds(points: &[Vec3]) -> (Vec3, Vec3) {
    let mut lo = Vec3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut hi = Vec3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in points {
        lo = Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
        hi = Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
    }
    (lo, hi)
}

fn dir_name(d: Vec3) -> String {
    let c = [(d.x, "x"), (d.y, "y"), (d.z, "z")];
    let (v, n) = c.iter().copied().fold((0.0f64, "z"), |best, (v, n)| if v.abs() > best.0.abs() { (v, n) } else { best });
    if c.iter().filter(|(x, _)| x.abs() > 1e-6).count() > 1 {
        return format!("[{:.3}, {:.3}, {:.3}]", d.x, d.y, d.z);
    }
    format!("{}{n}", if v < 0.0 { "-" } else { "+" })
}

fn frame_steps(directions: &[&'static str], held: &str) -> Vec<Step> {
    let mut dirs: Vec<&str> = directions.to_vec();
    for d in ["+z", "-z"] {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs.iter()
        .map(|d| Step { name: format!("From {d}"), direction: (*d).into(), held_by: held.into(), cuts: format!("every face reached from {d}") })
        .collect()
}

/// The cylinders and planes of the part, from the faces' exact surfaces.
fn surfaces(bodies: &[BodyInput]) -> (Vec<Cyl>, Vec<Flat>) {
    let mut cyls = Vec::new();
    let mut flats = Vec::new();
    for (bi, b) in bodies.iter().enumerate() {
        let mut tris: Vec<Vec<usize>> = vec![Vec::new(); b.surfaces.len()];
        for (ti, f) in b.mesh.tri_face.iter().enumerate() {
            if let Some(list) = tris.get_mut(*f as usize) {
                list.push(ti);
            }
        }
        for (face, surface) in b.surfaces.iter().enumerate() {
            let Some(ts) = tris.get(face) else { continue };
            if ts.is_empty() {
                continue;
            }
            match *surface {
                Surface::Cylinder { origin, axis, radius } => {
                    let Some(ax) = axis.normalized() else { continue };
                    if !(radius.is_finite() && radius > 0.0) {
                        continue;
                    }
                    let (mut a0, mut a1, mut inward) = (f64::INFINITY, f64::NEG_INFINITY, 0.0);
                    for t in ts {
                        let Some(tri) = b.mesh.triangles.get(*t).and_then(|t| b.mesh.tri(t)) else { continue };
                        let n2 = (tri[1] - tri[0]).cross(tri[2] - tri[0]);
                        let c = (tri[0] + tri[1] + tri[2]) * (1.0 / 3.0);
                        let d = c - origin;
                        let radial = d - ax * d.dot(ax);
                        inward -= n2.dot(radial);
                        for p in tri {
                            let a = (p - origin).dot(ax);
                            a0 = a0.min(a);
                            a1 = a1.max(a);
                        }
                    }
                    if !(a0.is_finite() && a1.is_finite()) {
                        continue;
                    }
                    let span = crate::analyze::angular_span(bodies, bi, ts, origin, ax);
                    cyls.push(Cyl { origin, axis: ax, radius, a0, a1, concave: inward > 0.0, full: span > 5.5 });
                }
                Surface::Plane { normal } => {
                    let Some(n) = normal.normalized() else { continue };
                    let mut area = 0.0;
                    let mut at = None;
                    for t in ts {
                        let Some(tri) = b.mesh.triangles.get(*t).and_then(|t| b.mesh.tri(t)) else { continue };
                        area += (tri[1] - tri[0]).cross(tri[2] - tri[0]).len() * 0.5;
                        at.get_or_insert(tri[0]);
                    }
                    if let Some(at) = at {
                        flats.push(Flat { normal: n, at, area, face, body: bi });
                    }
                }
                _ => {}
            }
        }
    }
    (cyls, flats)
}

/// A part held at one place and pushed sideways at its far end.
struct Beam<'a> {
    drop: &'a DropMesh,
    modulus: f64,
    force: f64,
    limit: f64,
    lo: Vec3,
    hi: Vec3,
}

/// How far the part bends when held at `grip` and pushed along `push` at its far end.
#[derive(Clone, Copy, Debug, Default)]
struct Bend {
    /// From the grip to the far end, along the part (mm).
    overhang: f64,
    /// Deflection at the far end under the finishing force (mm).
    deflection: f64,
    /// The thinnest section's thickness along `push` (mm) and where it is (mm from the grip).
    thinnest: f64,
    thinnest_at: f64,
    /// The weakest section's second moment of area (mm⁴).
    weakest_i: f64,
}

impl Beam<'_> {
    /// Held over the part's sections up to `from` along `along` (unit), pushed along `push`
    /// (unit, square to `along`); `across` completes the frame. The sections run from `from` to
    /// the far end; each is sampled across the part, its thickness along `push` measured with a
    /// thin probe from both sides.
    fn bend(&self, from: Vec3, along: Vec3, push: Vec3) -> Bend {
        let across = push.cross(along);
        let corners = [
            Vec3::new(self.lo.x, self.lo.y, self.lo.z),
            Vec3::new(self.hi.x, self.lo.y, self.lo.z),
            Vec3::new(self.lo.x, self.hi.y, self.lo.z),
            Vec3::new(self.hi.x, self.hi.y, self.lo.z),
            Vec3::new(self.lo.x, self.lo.y, self.hi.z),
            Vec3::new(self.hi.x, self.lo.y, self.hi.z),
            Vec3::new(self.lo.x, self.hi.y, self.hi.z),
            Vec3::new(self.hi.x, self.hi.y, self.hi.z),
        ];
        let reach = corners.iter().map(|c| (*c - from).dot(along)).fold(0.0f64, f64::max);
        let (c0, c1) = corners.iter().map(|c| (*c - from).dot(across)).fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(v), b.max(v)));
        let depth = corners.iter().map(|c| ((*c - from).dot(push)).abs()).fold(0.0f64, f64::max) + 1.0;
        let (Ok(up), Ok(down)) = (Frame::new(from, push, along), Frame::new(from, push * -1.0, along)) else { return Bend::default() };
        let probe = Tool::new(0, "probe", ToolKind::FlatEnd, 0.05);
        if !(reach > 0.0 && c1 > c0) {
            return Bend::default();
        }
        let ds = reach / STATIONS as f64;
        let dc = (c1 - c0) / ACROSS as f64;
        let mut sections: Vec<(f64, f64, f64)> = Vec::new();
        for k in 0..STATIONS {
            let s = (k as f64 + 0.5) * ds;
            let (mut i_sec, mut thick) = (0.0, 0.0f64);
            for j in 0..ACROSS {
                let c = c0 + (j as f64 + 0.5) * dc;
                // The frames' local x is `along`, y `push × along` (up) or its reverse (down).
                let xy_up = Vec2::new(s, up.to_local(from + across * c).y);
                let xy_down = Vec2::new(s, down.to_local(from + across * c).y);
                let top = self.drop.drop(&probe, &up, xy_up, -depth).map(|h| h.z);
                let bottom = self.drop.drop(&probe, &down, xy_down, -depth).map(|h| -h.z);
                if let (Some(t), Some(b)) = (top, bottom)
                    && t > b
                {
                    let h = t - b;
                    i_sec += dc * h * h * h / 12.0;
                    thick = thick.max(h);
                }
            }
            sections.push((s, i_sec, thick));
        }
        // Unit-load deflection of a cantilever: δ = F ∫ (L - s)² / (E I(s)) ds, over the sections
        // that carry material; the part's far end is its last section with any.
        let Some(end) = sections.iter().rev().find(|(_, i, _)| *i > 0.0).map(|(s, _, _)| *s + ds * 0.5) else { return Bend::default() };
        let mut delta = 0.0;
        let mut weakest = (f64::INFINITY, 0.0, 0.0);
        for (s, i, t) in &sections {
            if *s > end || *i <= 0.0 {
                continue;
            }
            delta += (end - s).powi(2) / (self.modulus * i) * ds;
            if *i < weakest.0 {
                weakest = (*i, *t, *s);
            }
        }
        Bend {
            overhang: end,
            deflection: self.force * delta,
            thinnest: weakest.1,
            thinnest_at: weakest.2,
            weakest_i: if weakest.0.is_finite() { weakest.0 } else { 0.0 },
        }
    }

    fn cost(&self, s: &mut Strategy, b: &Bend) {
        s.values.insert("overhang_mm".into(), json!(r3(b.overhang)));
        s.values.insert("deflection_mm".into(), json!(r3(b.deflection)));
        s.values.insert(
            "weakest_section_mm".into(),
            json!({"thickness": r3(b.thinnest), "from_grip": r3(b.thinnest_at), "second_moment_mm4": r3(b.weakest_i)}),
        );
        let pts = (b.deflection / self.limit * PER_DEFLECTION_LIMIT).min(DEFLECTION_CAP);
        cost(
            s,
            "deflection",
            pts,
            format!("the far end, {:.0} mm out, moves {:.3} mm under a {:.0} N finishing cut", b.overhang, b.deflection, self.force),
        );
        if b.deflection > self.limit {
            s.issues.push(
                Issue::warning(
                    "flex",
                    format!(
                        "held at one end the part bends {:.3} mm at its far end under a {:.0} N cut; weakest {:.1} mm thick, {:.0} mm from the grip: finish with light cuts from the tip back to the grip, or support the far end",
                        b.deflection, self.force, b.thinnest, b.thinnest_at
                    ),
                )
                .parameter("deflection_mm", r3(b.deflection))
                .limit(r3(self.limit)),
            );
        }
    }
}

/// Hold the part on a pin through one of its holes, its flat faces round the hole clamped.
fn pin_strategy(cyls: &[Cyl], flats: &[Flat], beam: &Beam, points: &[Vec3], analysis: &Analysis, tool_d: f64) -> Strategy {
    let mut s = strategy("pin", "Pin through a hole, boss clamped");
    // Candidate holes: concave, all the way round, at least 1.5 mm across.
    let mut best: Option<(Strategy, f64)> = None;
    for h in cyls.iter().filter(|c| c.concave && c.full && c.radius >= 0.75) {
        let ends = [h.origin + h.axis * h.a0, h.origin + h.axis * h.a1];
        // Flat faces square to the hole at its ends: the boss's sides the clamp bears on.
        let sides: Vec<&Flat> =
            flats.iter().filter(|f| f.normal.dot(h.axis).abs() > 0.999 && ends.iter().any(|e| (*e - f.at).dot(f.normal).abs() < 0.05)).collect();
        // A boss round it: a convex cylinder on the same axis.
        let boss = cyls
            .iter()
            .filter(|c| {
                !c.concave && c.axis.dot(h.axis).abs() > 0.999 && {
                    let d = c.origin - h.origin;
                    (d - h.axis * d.dot(h.axis)).len() < 0.05 + 0.01 * c.radius
                }
            })
            .map(|c| c.radius)
            .fold(0.0f64, f64::max);
        let rim = if boss > h.radius { boss } else { h.radius * 2.5 };
        let length = h.a1 - h.a0;
        // Which ends of the hole a flat face closes: two give a clamp both sides.
        let clamped = ends.iter().filter(|e| sides.iter().any(|f| (**e - f.at).dot(f.normal).abs() < 0.05)).count();
        // The part reaches out from the pin: along the farthest point's direction, square to the pin.
        let mid = h.origin + h.axis * ((h.a0 + h.a1) * 0.5);
        let far = points
            .iter()
            .map(|p| *p - mid)
            .map(|d| d - h.axis * d.dot(h.axis))
            .fold(Vec3::new(0.0, 0.0, 0.0), |a, d| if d.len() > a.len() { d } else { a });
        let Some(reach_dir) = far.normalized() else { continue };
        // Cuts push square to both: the tool axis of the setups that turn about the pin, the
        // frame's axis nearest it (a 3-axis machine cuts along its own axes); the part then
        // reaches out square to the pin and that axis.
        let Some(ideal) = h.axis.cross(reach_dir).normalized() else { continue };
        let push = [Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0)]
            .into_iter()
            .flat_map(|v| [v, v * -1.0])
            .filter(|v| v.dot(h.axis).abs() < 0.5 && v.dot(ideal) > 0.7)
            .max_by(|a, b| a.dot(ideal).total_cmp(&b.dot(ideal)))
            .unwrap_or(ideal);
        let Some(along) = push.cross(h.axis).normalized() else { continue };
        let along = if along.dot(reach_dir) < 0.0 { along * -1.0 } else { along };
        let bend = beam.bend(mid + along * rim, along, push);
        let mut c = strategy("pin", "Pin through a hole, boss clamped");
        c.feasible = clamped == 2 || (clamped == 1 && length >= 4.0);
        c.values.insert("hole".into(), json!({"diameter_mm": r3(h.radius * 2.0), "length_mm": r3(length), "axis": [r3(h.axis.x), r3(h.axis.y), r3(h.axis.z)], "centre": [r3(mid.x), r3(mid.y), r3(mid.z)]}));
        c.values.insert("boss_radius_mm".into(), json!(r3(rim)));
        c.values.insert(
            "clamp_faces".into(),
            json!(sides.iter().map(|f| json!({"body": f.body, "face": f.face, "area_mm2": r3(f.area)})).collect::<Vec<_>>()),
        );
        beam.cost(&mut c, &bend);
        // What stops the part turning on the pin is the clamp's friction: a bolt the hole's size
        // tightened, both clamped faces, at the washers' mean radius.
        let washer = (rim - 1.5).clamp(h.radius + 1.5, rim.max(h.radius + 1.5));
        let bolt = (h.radius * 2.0 - 0.3).max(1.0);
        let preload = 0.6 * BOLT_PROOF_MPA * std::f64::consts::FRAC_PI_4 * (0.82 * bolt).powi(2);
        let torque = clamped.max(1) as f64 * CLAMP_FRICTION * preload * (h.radius + washer) * 0.5 / 1000.0;
        let lever = rim + bend.overhang;
        let slip = if lever > 0.0 { torque * 1000.0 / lever } else { f64::INFINITY };
        c.values.insert(
            "clamp".into(),
            json!({"bolt_mm": r3(bolt), "preload_n": r3(preload), "torque_nm": r3(torque), "slip_force_at_far_end_n": r3(slip)}),
        );
        let need = ROUGHING_FORCE * TORQUE_MARGIN;
        if slip < need {
            c.issues.push(
                Issue::warning(
                    "pin_torque",
                    format!(
                        "the clamp holds {torque:.1} N·m: {slip:.0} N at the far end turns the part on the pin, under the {need:.0} N a roughing cut may push with a margin; add a stop against the part beside the boss, or rough with a light engagement"
                    ),
                )
                .parameter("slip_force_n", r3(slip))
                .limit(r3(need)),
            );
        }
        cost(
            &mut c,
            "torque",
            ((need / slip.max(1e-6) - 1.0) * PER_DEFLECTION_LIMIT).clamp(0.0, 20.0),
            format!("{slip:.0} N at the far end turns the part on the pin"),
        );
        let a = dir_name(h.axis);
        let na = dir_name(h.axis * -1.0);
        let t = dir_name(push);
        let nt = dir_name(push * -1.0);
        c.steps = vec![
            Step {
                name: format!("Boss from {a}"),
                direction: a.clone(),
                held_by: "the stock in a vise, by its far end".into(),
                cuts: format!("the boss's {a} side flat and the hole through, from the solid stock"),
            },
            Step {
                name: format!("Boss from {na}"),
                direction: na.clone(),
                held_by: "the stock in a vise, turned over".into(),
                cuts: format!("the boss's {na} side flat, to the boss's length, and a slot beside it through the stock for the post"),
            },
            Step {
                name: format!("Part from {t}"),
                direction: t.clone(),
                held_by: "a pin through the hole, the boss clamped between washers on a post".into(),
                cuts: format!("everything reached from {t}, the boss's rim included, keeping clear of the washers"),
            },
            Step {
                name: format!("Part from {nt}"),
                direction: nt.clone(),
                held_by: "the same pin, the part turned half round it".into(),
                cuts: format!("everything reached from {nt}; the part is free when it ends"),
            },
        ];
        // Faces reached only from directions square to the pin need no more setups: turning on
        // the pin reaches them. Directions along the pin are the boss steps.
        let extra: Vec<&str> = analysis
            .directions
            .iter()
            .copied()
            .filter(|d| {
                let v = match *d {
                    "+x" => Vec3::new(1.0, 0.0, 0.0),
                    "-x" => Vec3::new(-1.0, 0.0, 0.0),
                    "+y" => Vec3::new(0.0, 1.0, 0.0),
                    "-y" => Vec3::new(0.0, -1.0, 0.0),
                    "+z" => Vec3::new(0.0, 0.0, 1.0),
                    _ => Vec3::new(0.0, 0.0, -1.0),
                };
                v.dot(h.axis).abs() < 0.999 && v.dot(push).abs() < 0.999
            })
            .collect();
        for d in &extra {
            c.steps.push(Step {
                name: format!("Part from {d}"),
                direction: (*d).into(),
                held_by: "the pin, the part turned a quarter round it".into(),
                cuts: format!("the faces only {d} reaches"),
            });
        }
        let setups = c.steps.len();
        cost(&mut c, "hand_finish", 0.0, "nothing: the boss is finished before it is clamped and the part comes off the pin finished".into());
        cost(&mut c, "setups", setups as f64 * PER_SETUP, format!("{setups} setups"));
        cost(&mut c, "extra_cut", 0.0, "none: only the part's own stock is cut".into());
        cost(&mut c, "location", 0.0, "the pin finds the part again exactly each time it turns".into());
        cost(
            &mut c,
            "fixture",
            FIXTURE_TO_MAKE,
            format!(
                "a post with a {:.1} mm pin and two washers, under {:.1} mm across so the boss's rim can be finished",
                h.radius * 2.0,
                (rim - tool_d * 0.25).max(h.radius + 1.5) * 2.0
            ),
        );
        // The fixture bodies: the pin, a washer each side, a post below each turned setup.
        let wt = 3.0;
        let pin_len = length + 2.0 * wt;
        let base = h.origin + h.axis * (h.a0 - wt);
        c.fixtures.push(Fixture::Cylinder {
            name: "Pin".into(),
            base: v3(base),
            axis: v3(h.axis),
            radius: r3((h.radius - 0.02).max(0.1)),
            length: r3(pin_len),
            setups: vec![format!("Part from {t}"), format!("Part from {nt}")],
        });
        for (end, out) in [(h.a0, -1.0), (h.a1, 1.0)] {
            let at = h.origin + h.axis * (end + if out < 0.0 { -wt } else { 0.0 });
            c.fixtures.push(Fixture::Cylinder {
                name: format!("Washer {}", if out < 0.0 { &na } else { &a }),
                base: v3(at),
                axis: v3(h.axis),
                radius: r3(washer),
                length: wt,
                setups: vec![format!("Part from {t}"), format!("Part from {nt}")],
            });
        }
        // The post stands on the table beside the boss's a0 end, carrying the pin: in each
        // turned setup it reaches from the pin away from the tool, past the part.
        for (name, tool_axis) in [(&t, push), (&nt, push * -1.0)] {
            let below = points.iter().map(|p| (*p - mid).dot(tool_axis * -1.0)).fold(0.0f64, f64::max) + POST_BELOW;
            let a_lo = h.a0 - wt - POST_THICKNESS;
            let a_hi = h.a0 - wt;
            let mut lo = Vec3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
            let mut hi = Vec3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
            for ax in [a_lo, a_hi] {
                for u in [-washer, washer] {
                    for w in [0.0, -below] {
                        let p = h.origin + h.axis * ax + (mid - h.origin - h.axis * (mid - h.origin).dot(h.axis)) + along * u + tool_axis * w;
                        lo = Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
                        hi = Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
                    }
                }
            }
            c.fixtures.push(Fixture::Box {
                name: format!("Post (part from {name})"),
                min: v3(lo),
                max: v3(hi),
                setups: vec![format!("Part from {name}")],
            });
        }
        c.why = if c.feasible {
            format!(
                "the {:.1} mm hole through the boss takes a pin and {} take a clamp, so the part is held by its own boss, found again exactly when turned, and nothing is left to file",
                h.radius * 2.0,
                if clamped == 2 { "the boss's flat sides at both ends" } else { "the boss's flat side at one end" }
            )
        } else {
            format!("the {:.1} mm hole has no flat side square to it for a clamp to bear on", h.radius * 2.0)
        };
        let score = c.costs.values().filter_map(|v| v.get("points").and_then(Value::as_f64)).sum::<f64>();
        if c.feasible && best.as_ref().is_none_or(|(b, sc)| !b.feasible || score < *sc) || best.is_none() {
            best = Some((c, score));
        }
    }
    match best {
        Some((c, _)) => c,
        None => {
            s.why = "the part has no hole all the way round to take a pin".into();
            s
        }
    }
}

fn v3(p: Vec3) -> [f64; 3] {
    [r3(p.x), r3(p.y), r3(p.z)]
}

/// A vise on two parallel faces of the part, facing apart, that overlap enough to grip.
fn vise_strategy(flats: &[Flat], bodies: &[BodyInput], cyls: &[Cyl], analysis: &Analysis) -> Strategy {
    let mut s = strategy("vise", "Vise on the part's parallel faces");
    // Faces round a hole belong to the pin strategy.
    let near_hole = |f: &Flat| {
        cyls.iter().any(|c| {
            c.concave
                && c.full
                && c.axis.dot(f.normal).abs() > 0.999
                && [c.a0, c.a1].iter().any(|a| (c.origin + c.axis * *a - f.at).dot(f.normal).abs() < 0.05)
        })
    };
    let mut best: Option<(f64, f64, f64, &Flat, &Flat)> = None;
    for (i, f) in flats.iter().enumerate() {
        for g in flats.iter().skip(i + 1) {
            // A vise's jaws close square to the tool axis: they grip walls, and the tool comes
            // from above (+z) and, the part turned over, from below.
            if f.normal.dot(g.normal) > -0.999 || f.normal.z.abs() > 0.02 || near_hole(f) || near_hole(g) {
                continue;
            }
            // They face apart: g lies behind f.
            let gap = (f.at - g.at).dot(f.normal);
            if gap <= 0.0 {
                continue;
            }
            let (w, h) = overlap(bodies, f, g);
            if w >= MIN_GRIP && h >= MIN_GRIP && best.is_none_or(|b| w * h > b.0) {
                best = Some((w * h, w.min(h), gap, f, g));
            }
        }
    }
    match best {
        Some((area, grip, gap, f, _)) => {
            s.feasible = true;
            s.why = format!(
                "two parallel faces {gap:.1} mm apart overlap {area:.0} mm² with at least {grip:.1} mm of grip: a vise holds the part by them"
            );
            s.values.insert("jaw_gap_mm".into(), json!(r3(gap)));
            s.values.insert("grip_mm".into(), json!(r3(grip)));
            let (n, nn) = (dir_name(f.normal), dir_name(f.normal * -1.0));
            s.steps.push(Step {
                name: "Walls to grip".into(),
                direction: "+z".into(),
                held_by: "the stock in a vise".into(),
                cuts: format!("the part's {n} and {nn} walls, square and parallel, and everything else reached from +z above the jaws"),
            });
            s.steps.push(Step {
                name: "From -z".into(),
                direction: "-z".into(),
                held_by: format!("a vise on the part's {n} and {nn} walls, turned over"),
                cuts: "everything reached from -z".into(),
            });
            for d in analysis.directions.iter().copied().filter(|d| !d.ends_with('z') && *d != n && *d != nn) {
                s.steps.push(Step {
                    name: format!("From {d}"),
                    direction: d.into(),
                    held_by: "a vise on the part's walls, stood on end".into(),
                    cuts: format!("the faces only {d} reaches"),
                });
            }
            let setups = s.steps.len();
            cost(&mut s, "hand_finish", 0.0, "nothing".into());
            cost(&mut s, "setups", setups as f64 * PER_SETUP, format!("{setups} setups"));
            cost(&mut s, "fixture", FIXTURE_TO_MAKE * 0.5, "soft jaws, or plain jaws with care for the faces".into());
        }
        None => s.why = "the part has no two parallel walls facing apart with 3 mm of grip on each for a vise's jaws".into(),
    }
    s
}

/// The overlap of two parallel faces seen along their normal: its width and height (mm).
fn overlap(bodies: &[BodyInput], f: &Flat, g: &Flat) -> (f64, f64) {
    let e1 = f.normal.any_perp().normalized().unwrap_or(Vec3::new(1.0, 0.0, 0.0));
    let e2 = f.normal.cross(e1);
    let extent = |fl: &Flat| {
        let mut b = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        if let Some(body) = bodies.get(fl.body) {
            for (ti, face) in body.mesh.tri_face.iter().enumerate() {
                if *face as usize != fl.face {
                    continue;
                }
                let Some(tri) = body.mesh.triangles.get(ti).and_then(|t| body.mesh.tri(t)) else { continue };
                for p in tri {
                    let (u, v) = (p.dot(e1), p.dot(e2));
                    b = (b.0.min(u), b.1.max(u), b.2.min(v), b.3.max(v));
                }
            }
        }
        b
    };
    let (a, b) = (extent(f), extent(g));
    ((a.1.min(b.1) - a.0.max(b.0)).max(0.0), (a.3.min(b.3) - a.2.max(b.2)).max(0.0))
}

/// The end of the part's long axis to grow a stub from: the end whose section is stiffer.
#[derive(Clone, Copy, Debug)]
struct StubEnd {
    /// Where the stub meets the part, and the direction it grows (out of the part).
    root: Vec3,
    out: Vec3,
    bend: Bend,
}

fn stub_end(beam: &Beam, lo: Vec3, hi: Vec3) -> StubEnd {
    let size = hi - lo;
    let mid = (lo + hi) * 0.5;
    let long = if size.x >= size.y { Vec3::new(1.0, 0.0, 0.0) } else { Vec3::new(0.0, 1.0, 0.0) };
    let half = if size.x >= size.y { size.x * 0.5 } else { size.y * 0.5 };
    let push = Vec3::new(0.0, 0.0, 1.0);
    let mut best: Option<StubEnd> = None;
    for sign in [1.0, -1.0] {
        let out = long * sign;
        let root = mid + out * half;
        // Held at this end, the part reaches back along -out.
        let bend = beam.bend(root, out * -1.0, push);
        if best.is_none_or(|b| bend.deflection < b.bend.deflection) {
            best = Some(StubEnd { root, out, bend });
        }
    }
    best.unwrap_or(StubEnd { root: mid, out: long, bend: Bend::default() })
}

/// A stub of stock left on one end of the part, gripped in a vise, cut off at the end.
fn stub_strategy(stub: &StubEnd, beam: &Beam, analysis: &Analysis) -> Strategy {
    let mut s = strategy("stub", "Stub of stock in a vise");
    s.feasible = true;
    let end = dir_name(stub.out);
    s.why = format!("a {STUB_SECTION:.0} mm stub of stock left on the {end} end is gripped in a vise; one spot is cut off and filed at the end");
    s.values.insert("stub_end".into(), json!(end));
    s.values.insert("stub_mm".into(), json!({"grip": STUB_GRIP, "section": STUB_SECTION}));
    beam.cost(&mut s, &stub.bend);
    let dirs: Vec<&str> = analysis.directions.iter().copied().filter(|d| *d != end).collect();
    let mut steps: Vec<Step> = vec![
        Step {
            name: "From +z".into(),
            direction: "+z".into(),
            held_by: format!("the stub in a vise, the part reaching out along {}", dir_name(stub.out * -1.0)),
            cuts: "everything reached from +z".into(),
        },
        Step {
            name: "From -z".into(),
            direction: "-z".into(),
            held_by: "the stub, turned half round".into(),
            cuts: "everything reached from -z".into(),
        },
    ];
    for d in dirs.iter().filter(|d| !d.ends_with('z')) {
        steps.push(Step {
            name: format!("From {d}"),
            direction: (*d).into(),
            held_by: "the stub, turned a quarter round".into(),
            cuts: format!("the faces only {d} reaches"),
        });
    }
    let n = steps.len();
    s.steps = steps;
    s.hand_finish = vec![format!("the stub's root on the {end} end")];
    cost(&mut s, "hand_finish", PER_HAND_SPOT, "one stub cut off and filed".into());
    cost(&mut s, "setups", n as f64 * PER_SETUP, format!("{n} setups"));
    cost(&mut s, "extra_cut", STUB_SECTION * STUB_SECTION * (STUB_GRIP + 6.0) / 1000.0 * PER_EXTRA_CM3, "the stub's own stock".into());
    cost(&mut s, "location", LOOSE_LOCATION * 0.5, "the vise grips the stub again; a stop finds its length".into());
    s.fixtures.push(Fixture::Box {
        name: "Vise jaws (stub)".into(),
        min: v3(stub.root + stub.out * 6.0 - Vec3::new(STUB_SECTION, STUB_SECTION, STUB_SECTION)),
        max: v3(stub.root + stub.out * (6.0 + STUB_GRIP) + Vec3::new(STUB_SECTION, STUB_SECTION, STUB_SECTION)),
        setups: vec!["From +z".into(), "From -z".into()],
    });
    s
}

/// The stub in a rotary module's chuck: every side in one setup.
fn rotary_strategy(stub: &StubEnd, beam: &Beam, machine: &Machine, points: &[Vec3]) -> Strategy {
    let mut s = strategy("rotary", "Stub in the rotary chuck");
    let rotary = matches!(machine.kinematics, Kinematics::FourAxis { .. }) || machine.rotary_capacity.is_some();
    // The part's swing round the long axis through the stub.
    let swing = points
        .iter()
        .map(|p| {
            let d = *p - stub.root;
            (d - stub.out * d.dot(stub.out)).len()
        })
        .fold(0.0f64, f64::max);
    let length = points.iter().map(|p| (stub.root - *p).dot(stub.out)).fold(0.0f64, f64::max) + STUB_GRIP + 6.0;
    s.values.insert("swing_diameter_mm".into(), json!(r3(swing * 2.0)));
    s.values.insert("length_mm".into(), json!(r3(length)));
    if !rotary {
        s.why = format!("`{}` has no rotary module; with one (a _4axis machine) a stub in the chuck reaches every side in one setup", machine.name);
        s.issues.push(Issue::info("no_rotary", s.why.clone()));
        return s;
    }
    if let Some((d, l)) = machine.rotary_capacity
        && (swing * 2.0 > d || length > l)
    {
        s.why = format!(
            "the part swings {:.0} mm across and is {:.0} mm long with its stub; the rotary module takes {d:.0} × {l:.0} mm",
            swing * 2.0,
            length
        );
        s.issues.push(Issue::info("too_big_for_rotary", s.why.clone()).limit(json!([d, l])));
        return s;
    }
    s.feasible = true;
    s.why = "a stub in the rotary chuck turns the part to every side in one setup, found again exactly each turn".into();
    beam.cost(&mut s, &stub.bend);
    s.steps = vec![Step {
        name: "Rotary".into(),
        direction: format!("round {}", dir_name(stub.out)),
        held_by: "the stub in the chuck".into(),
        cuts: "every side, indexed or continuous".into(),
    }];
    s.hand_finish = vec![format!("the stub's root on the {} end", dir_name(stub.out))];
    cost(&mut s, "hand_finish", PER_HAND_SPOT, "one stub cut off and filed".into());
    cost(&mut s, "setups", PER_SETUP, "1 setup".into());
    cost(&mut s, "extra_cut", STUB_SECTION * STUB_SECTION * (STUB_GRIP + 6.0) / 1000.0 * PER_EXTRA_CM3, "the stub's own stock".into());
    cost(&mut s, "location", 0.0, "the chuck keeps the part's place through every turn".into());
    s
}
