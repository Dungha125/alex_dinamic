//! The Alex brand mark and wordmark, drawn as gradient vector polygons
//! (same geometry as tools/make_logo.py), so no image decoding is needed.
use eframe::egui::{epaint::Mesh, Color32, Painter, Pos2, Rect, Shape};

pub const C1: Color32 = Color32::from_rgb(0, 229, 255);
pub const C2: Color32 = Color32::from_rgb(41, 121, 255);
pub const C3: Color32 = Color32::from_rgb(170, 0, 255);
pub const BG: Color32 = Color32::from_rgb(8, 12, 26);

pub fn grad(t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let (a, b, u) = if t < 0.5 { (C1, C2, t * 2.0) } else { (C2, C3, (t - 0.5) * 2.0) };
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * u) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

type Poly = &'static [(f32, f32)];

// Glyphs on a 100-high grid, split into convex pieces.
const A: &[Poly] = &[
    &[(0., 100.), (37., 0.), (50., 0.), (50., 32.), (26., 100.)],
    &[(100., 100.), (74., 100.), (50., 32.), (50., 0.), (63., 0.)],
    &[(38., 70.), (62., 70.), (66., 84.), (34., 84.)],
];
const L: &[Poly] = &[&[(0., 0.), (22., 0.), (22., 100.), (10., 100.), (0., 90.)], &[(22., 78.), (72., 78.), (72., 100.), (22., 100.)]];
const E: &[Poly] = &[
    &[(0., 10.), (10., 0.), (22., 0.), (22., 100.), (10., 100.), (0., 90.)],
    &[(22., 0.), (72., 0.), (72., 22.), (22., 22.)],
    &[(22., 78.), (72., 78.), (72., 100.), (22., 100.)],
    &[(30., 39.), (64., 39.), (64., 61.), (30., 61.)],
];
const X: &[Poly] = &[&[(0., 0.), (27., 0.), (84., 100.), (57., 100.)], &[(57., 0.), (84., 0.), (27., 100.), (0., 100.)]];
const WORD: &[(&[Poly], f32)] = &[(A, 0.), (L, 116.), (E, 204.), (X, 292.)];
pub const WORD_W: f32 = 376.0;

const MARK: &[Poly] = &[
    &[(18., 82.), (42., 18.), (50., 18.), (50., 38.), (34., 82.)],
    &[(82., 82.), (66., 82.), (50., 38.), (50., 18.), (58., 18.)],
    &[(41., 62.), (59., 62.), (62., 72.), (38., 72.)],
];

fn hexagon(r: f32) -> Vec<(f32, f32)> {
    (0..6)
        .map(|i| {
            let a = (-90.0 + 60.0 * i as f32).to_radians();
            (50.0 + r * a.cos(), 50.0 + r * a.sin())
        })
        .collect()
}

/// Fan-triangulate a convex polygon into `mesh`, coloured by a diagonal gradient.
fn push(mesh: &mut Mesh, pts: &[(f32, f32)], map: impl Fn(f32, f32) -> Pos2, color: impl Fn(f32, f32) -> Color32) {
    let base = mesh.vertices.len() as u32;
    for &(x, y) in pts {
        mesh.colored_vertex(map(x, y), color(x, y));
    }
    for i in 1..pts.len() as u32 - 1 {
        mesh.add_triangle(base, base + i, base + i + 1);
    }
}

/// Hexagon badge with the chevron "A".
pub fn paint_mark(p: &Painter, rect: Rect, alpha: f32) {
    let s = rect.width().min(rect.height()) / 100.0;
    let o = rect.center() - eframe::egui::vec2(50.0 * s, 50.0 * s);
    let map = |x: f32, y: f32| Pos2::new(o.x + x * s, o.y + y * s);
    let g = |x: f32, y: f32| grad((x + y) / 200.0).gamma_multiply(alpha);
    let mut m = Mesh::default();
    push(&mut m, &hexagon(48.0), map, g);
    push(&mut m, &hexagon(42.0), map, |_, _| BG.gamma_multiply(alpha));
    for poly in MARK {
        push(&mut m, poly, map, g);
    }
    p.add(Shape::mesh(m));
}

/// "ALEX" wordmark; `h` is the cap height in points. Returns the width used.
pub fn paint_word(p: &Painter, left_top: Pos2, h: f32, alpha: f32) -> f32 {
    let s = h / 100.0;
    let mut m = Mesh::default();
    for (glyph, ox) in WORD {
        for poly in *glyph {
            push(
                &mut m,
                poly,
                |x, y| Pos2::new(left_top.x + (ox + x) * s, left_top.y + y * s),
                |x, y| grad(((ox + x) / WORD_W + y / 100.0) / 2.0).gamma_multiply(alpha),
            );
        }
    }
    p.add(Shape::mesh(m));
    WORD_W * s
}
