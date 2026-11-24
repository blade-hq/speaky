//! Paint the tour over the actual desktop content. This layer has no input handlers.
use gpui::{
    canvas, fill, linear_color_stop, linear_gradient, point, prelude::*, px, rgb, size, Bounds,
    BoxShadow, Hsla, PathBuilder, Pixels, Point, Window,
};

use super::motion::{ease_out, lerp, Frame, Scene};

pub(super) const MINT: u32 = 0x8ee4bc;
pub(super) const GOLD: u32 = 0xffcf91;

pub(super) fn backdrop(
    window: &mut Window,
    viewport: Bounds<Pixels>,
    focus: Option<Bounds<Pixels>>,
    holes: &[Bounds<Pixels>],
    shade: f32,
) {
    if shade <= 0.002 {
        return;
    }
    let background = linear_gradient(
        145.,
        linear_color_stop(tint(0x10211c).opacity(shade), 0.),
        linear_color_stop(tint(0x091c19).opacity(shade * 0.86), 1.),
    );
    let mut mask = PathBuilder::fill();
    rounded_rect(&mut mask, viewport, px(0.));
    for hole in holes {
        // An even-odd path makes each opening a single shape without seams.
        let radius = if focus == Some(*hole) {
            px(10.)
        } else {
            px(7.)
        };
        rounded_rect(&mut mask, *hole, radius);
    }
    if let Ok(path) = mask.build() {
        window.paint_path(path, background);
    }
}

pub(super) fn lighting(
    window: &mut Window,
    frame: Frame,
    focus: Bounds<Pixels>,
    extras: &[Bounds<Pixels>],
    record_button: Option<Bounds<Pixels>>,
    ring: f32,
) {
    if ring <= 0.003 {
        return;
    }
    let strength = ring * if frame.moving { 0.85 } else { 0.65 };
    if frame.scene == Scene::Welcome {
        let spread = ease_out(frame.part((40., 830.)));
        let halo = centered(
            focus.center(),
            px(110. + 238. * spread),
            px(44. + 83. * spread),
        );
        glow(
            window,
            halo,
            px(35. + 54. * spread),
            tint(MINT).opacity(strength * 0.23),
        );
        glow(
            window,
            halo + point(px(58.), px(-36.)),
            px(55.),
            tint(GOLD).opacity(strength * 0.10),
        );
    } else {
        glow(window, focus, px(29.), tint(MINT).opacity(strength * 0.18));
        if frame.scene == Scene::Model {
            let corner = centered(
                point(focus.right() - px(14.), focus.top() + px(8.)),
                px(70.),
                px(20.),
            );
            glow(window, corner, px(31.), tint(GOLD).opacity(strength * 0.20));
        }
        if matches!(frame.scene, Scene::Record | Scene::Live) {
            if let Some(button) = record_button {
                let breath = if frame.moving && frame.scene == Scene::Record {
                    let beat = frame.part((190., 1430.));
                    (beat * std::f32::consts::TAU).sin().abs() * (1. - beat * 0.29)
                } else {
                    0.
                };
                glow(
                    window,
                    button.dilate(px(2. + 5. * breath)),
                    px(10. + 11. * breath),
                    tint(MINT).opacity(strength * (0.10 + breath * 0.38)),
                );
            }
        }
    }
    trim(window, focus, tint(0x3d8067).opacity(ring * 0.94), px(10.));
    trim(
        window,
        focus.dilate(px(2.)),
        tint(MINT).opacity(ring * 0.62),
        px(12.),
    );
    for extra in extras {
        trim(window, *extra, tint(MINT).opacity(ring * 0.58), px(9.));
    }
}

pub(super) fn ribbon(
    window: &mut Window,
    frame: Frame,
    voice: Bounds<Pixels>,
    text: Bounds<Pixels>,
) {
    if !frame.moving
        || frame.scene != Scene::Transcript
        || !matches!(frame.previous, Scene::Record | Scene::Live)
    {
        return;
    }
    let head = ease_out(frame.part((95., 690.)));
    let strength = frame.part((75., 165.)) * (1. - frame.part((530., 780.)));
    if strength <= 0.01 {
        return;
    }
    let from = voice.center();
    let to = point(text.left() + text.size.width * 0.22, text.top() + px(35.));
    let middle = point(from.x - px(22.), (from.y + to.y) / 2. + px(35.));
    let mut path = PathBuilder::stroke(px(1.55));
    let mut echo = PathBuilder::stroke(px(3.5));
    let tail = (head - 0.30).max(0.);
    for sample in 0..=28 {
        let local = sample as f32 / 28.;
        let t = lerp(tail, head, local);
        let u = 1. - t;
        let wave = (local * 22. - head * 9.).sin() * (std::f32::consts::PI * local).sin();
        let p = point(
            from.x * (u * u) + middle.x * (2. * u * t) + to.x * (t * t),
            from.y * (u * u) + middle.y * (2. * u * t) + to.y * (t * t) + px(9. * wave * u),
        );
        if sample == 0 {
            path.move_to(p);
            echo.move_to(p);
        } else {
            path.line_to(p);
            echo.line_to(p);
        }
    }
    if let Ok(echo) = echo.build() {
        window.paint_path(echo, tint(MINT).opacity(strength * 0.24));
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, tint(GOLD).opacity(strength * 0.93));
    }
}

pub(super) fn signal(frame: Frame, height: f32, hero: bool) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let height = bounds.size.height;
            let middle = bounds.center().y;
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.left(), middle),
                    size(bounds.size.width, px(1.)),
                ),
                tint(if hero { MINT } else { 0x3f8065 }).opacity(0.19),
            ));
            let count = if hero { 41 } else { 29 };
            let color = tint(if hero { MINT } else { 0x3b8164 });
            let travel = frame.part(if hero { (90., 870.) } else { (235., 1050.) });
            for index in 0..count {
                let u = index as f32 / (count - 1) as f32;
                let form = (std::f32::consts::PI * u).sin().powi(2);
                let beat = (u * 32. + (1. - travel) * 10.).sin().abs();
                let front = if frame.moving {
                    ((travel * 1.55 - u) * 5.).clamp(0., 1.)
                } else {
                    1.
                };
                let amplitude = px(2.) + height * (0.69 * form * (0.32 + 0.68 * beat) * front);
                let x = bounds.left() + bounds.size.width * u - px(1.);
                window.paint_quad(
                    fill(
                        Bounds::new(
                            point(x, middle - amplitude / 2.),
                            size(px(if hero { 2.5 } else { 2. }), amplitude),
                        ),
                        color.opacity((0.25 + 0.66 * form) * (0.28 + 0.72 * front)),
                    )
                    .corner_radii(px(2.)),
                );
            }
        },
    )
    .w_full()
    .h(px(height))
}

fn rounded_rect(path: &mut PathBuilder, bounds: Bounds<Pixels>, radius: Pixels) {
    let r = radius
        .min(bounds.size.width / 2.)
        .min(bounds.size.height / 2.)
        .max(px(0.));
    let (l, t, right, b) = (bounds.left(), bounds.top(), bounds.right(), bounds.bottom());
    path.move_to(point(l + r, t));
    path.line_to(point(right - r, t));
    if r > px(0.) {
        path.curve_to(point(right, t + r), point(right, t));
    }
    path.line_to(point(right, b - r));
    if r > px(0.) {
        path.curve_to(point(right - r, b), point(right, b));
    }
    path.line_to(point(l + r, b));
    if r > px(0.) {
        path.curve_to(point(l, b - r), point(l, b));
    }
    path.line_to(point(l, t + r));
    if r > px(0.) {
        path.curve_to(point(l + r, t), point(l, t));
    }
    path.close();
}

fn trim(window: &mut Window, bounds: Bounds<Pixels>, color: Hsla, radius: Pixels) {
    window.paint_quad(gpui::outline(bounds, color, gpui::BorderStyle::Solid).corner_radii(radius));
}

fn glow(window: &mut Window, bounds: Bounds<Pixels>, blur: Pixels, color: Hsla) {
    window.paint_shadows(
        bounds,
        px(12.).into(),
        &[BoxShadow {
            color,
            offset: point(px(0.), px(0.)),
            blur_radius: blur,
            spread_radius: px(0.),
        }],
    );
}

pub(super) fn centered(center: Point<Pixels>, width: Pixels, height: Pixels) -> Bounds<Pixels> {
    Bounds::new(
        point(center.x - width / 2., center.y - height / 2.),
        size(width, height),
    )
}

fn tint(hex: u32) -> Hsla {
    rgb(hex).into()
}
