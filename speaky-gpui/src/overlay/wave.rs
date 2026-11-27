use crate::theme;
use gpui::{canvas, linear_color_stop, linear_gradient, point, prelude::*, px, rgb, PathBuilder};
use speaky_core::wave::{VoiceWave, COLORS};

pub(super) fn element(wave: VoiceWave) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let colors = if theme::current() == theme::Theme::Light {
                [0x247a70, 0x356a95, 0x7650a0, 0xa83f75, 0xa56528]
            } else {
                COLORS
            };
            for echo in [true, false] {
                let Some(curve) =
                    wave.sample(bounds.size.width.into(), bounds.size.height.into(), echo)
                else {
                    continue;
                };
                for (points, colors) in curve.spans.iter().zip(colors.windows(2)) {
                    let mut path = PathBuilder::stroke(px(curve.thickness));
                    let mut points = points
                        .iter()
                        .map(|&(x, y)| bounds.origin + point(px(x), px(y)));
                    path.move_to(points.next().expect("wave span has a starting point"));
                    for point in points {
                        path.line_to(point);
                    }
                    if let Ok(path) = path.build() {
                        let gradient = linear_gradient(
                            90.,
                            linear_color_stop(rgb(colors[0]), 0.),
                            linear_color_stop(rgb(colors[1]), 1.),
                        )
                        .opacity(curve.opacity);
                        window.paint_path(path, gradient);
                    }
                }
            }
        },
    )
    .w_full()
    .h(px(28.))
    .flex_none()
}
