use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Instant,
};

use gpui::{
    canvas, div, point, prelude::*, px, size, AnyElement, AvailableSpace, Bounds, Canvas, Div,
    Pixels, Point, ScrollHandle, Size,
};

use super::{
    effects,
    motion::{self, Frame, Scene},
};

const MARGIN: Pixels = px(16.);
const GAP: Pixels = px(13.);
pub(super) const CARD_WIDTH: Pixels = px(386.);
pub(super) const WELCOME_WIDTH: Pixels = px(496.);

#[derive(Clone, Copy)]
pub(crate) enum Anchor {
    Workspace,
    Input,
    Model,
    Service,
    Record,
    RecordButton,
    Transcript,
    HistoryLink,
    History,
    SettingsError,
    Status,
    Dock,
}

#[derive(Clone, Copy)]
struct Target {
    bounds: Bounds<Pixels>,
    clip: Bounds<Pixels>,
}

type Measurements = [Option<Target>; Anchor::Dock as usize + 1];

#[derive(Default)]
pub(super) struct Layout {
    measurements: Rc<Cell<Measurements>>,
    force_dock: Rc<Cell<bool>>,
    reveal: Rc<Cell<bool>>,
    viewport: Option<Size<Pixels>>,
    pub(super) scroll: ScrollHandle,
    camera: Rc<RefCell<Camera>>,
}

impl Layout {
    pub(super) fn reset(&mut self) {
        self.force_dock.set(false);
        self.reveal.set(true);
        self.scroll.set_offset(point(px(0.), px(0.)));
    }

    pub(super) fn reveal(&self) {
        self.reveal.set(true);
    }

    pub(super) fn clear_camera(&self) {
        *self.camera.borrow_mut() = Camera::default();
    }

    pub(super) fn docked(&mut self, viewport: Size<Pixels>) -> bool {
        if self.viewport != Some(viewport) {
            self.viewport = Some(viewport);
            self.force_dock.set(false);
            self.reveal();
        }
        self.force_dock.get() || viewport.width < px(820.) || viewport.height < px(525.)
    }

    pub(super) fn mark(&self, anchor: Anchor) -> Canvas<()> {
        let measurements = self.measurements.clone();
        canvas(
            move |bounds, window, _| {
                let mut frame = measurements.get();
                frame[anchor as usize] = Some(Target {
                    bounds,
                    clip: window.content_mask().bounds,
                });
                measurements.set(frame);
            },
            |_, (), _, _| {},
        )
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .size_full()
    }

    pub(super) fn spotlight(
        &self,
        cue: Frame,
        primary: Anchor,
        companion: Option<Anchor>,
        history_link: bool,
        status: bool,
        card: Option<AnyElement>,
    ) -> impl IntoElement {
        let measurements = self.measurements.clone();
        let fallback = self.force_dock.clone();
        let reveal = self.reveal.clone();
        let scroll = self.scroll.clone();
        let camera = self.camera.clone();
        canvas(
            move |viewport, window, cx| {
                let frame = measurements.take();
                let target = frame[primary as usize].or(frame[Anchor::SettingsError as usize]);
                if reveal.replace(false) {
                    if let Some(target) = target {
                        if let Some(offset) = scroll_to_target(&scroll, target.bounds) {
                            scroll.set_offset(offset);
                            window.on_next_frame(|window, _| window.refresh());
                        }
                    }
                }

                let visible = |anchor: Anchor| {
                    frame[anchor as usize].and_then(|target| visible_target(target, viewport))
                };
                let focus = target.and_then(|target| visible_target(target, viewport));
                let extras: Vec<_> = companion
                    .into_iter()
                    .chain(history_link.then_some(Anchor::HistoryLink))
                    .chain(status.then_some(Anchor::Status))
                    .filter_map(visible)
                    .collect();
                let protected: Vec<_> = focus.into_iter().chain(extras.iter().copied()).collect();
                let dock =
                    frame[Anchor::Dock as usize].map(|dock| dock.bounds.intersect(&viewport));

                // The cutouts leave the actual controls hit-testable. Scroll events can
                // still reach the page beneath the shaded areas.
                let mut blockers = div().size_full();
                if cue.scene != Scene::Exit {
                    for bounds in subtract_holes(viewport, protected.iter().copied().chain(dock)) {
                        blockers = blockers
                            .child(positioned(bounds, viewport.origin).block_mouse_except_scroll());
                    }
                }
                let mut blockers = blockers.into_any_element();
                blockers.prepaint_as_root(
                    viewport.origin,
                    viewport.size.map(AvailableSpace::Definite),
                    window,
                    cx,
                );

                let slot = card.and_then(|mut card| {
                    let dimensions = card.layout_as_root(
                        size(
                            AvailableSpace::Definite(viewport.size.width - MARGIN * 2.),
                            AvailableSpace::MinContent,
                        ),
                        window,
                        cx,
                    );
                    let origin = if cue.scene == Scene::Welcome {
                        Some(
                            effects::centered(
                                viewport.center(),
                                dimensions.width,
                                dimensions.height,
                            )
                            .origin,
                        )
                    } else {
                        focus.and_then(|target| {
                            place_card(
                                viewport,
                                target,
                                &protected,
                                dimensions,
                                cue.scene == Scene::HistoryLink,
                            )
                        })
                    };
                    if let Some(origin) = origin {
                        Some((card, Bounds::new(origin, dimensions)))
                    } else {
                        // Keep the dock for this step until it changes or the window resizes.
                        fallback.set(true);
                        reveal.set(true);
                        window.on_next_frame(|window, _| window.refresh());
                        None
                    }
                });

                let mut camera = camera.borrow_mut();
                let mut pose = camera.advance(
                    cue,
                    viewport,
                    focus,
                    slot.as_ref().map(|(_, goal)| goal.origin),
                );
                if pose.catching_up {
                    window.request_animation_frame();
                }
                let card = slot.map(|(mut card, goal)| {
                    let origin = slide_card(
                        goal,
                        pose.card_from,
                        cue.card(),
                        viewport,
                        &protected,
                        cue.scene == Scene::Welcome,
                    );
                    card.prepaint_at(origin, window, cx);
                    pose.card = Some(origin);
                    card
                });
                camera.shown = Some(pose);

                let mut holes = Vec::with_capacity(4);
                holes.extend(pose.focus);
                for extra in extras.iter().copied().chain(dock) {
                    if !holes.iter().any(|hole| hole.intersects(&extra)) {
                        holes.push(extra);
                    } else if Some(extra) == dock {
                        // The dock must remain unshaded, including during a move.
                        holes.retain(|hole| !hole.intersects(&extra));
                        holes.push(extra);
                    }
                }
                let record = visible(Anchor::RecordButton);
                let text = visible(Anchor::Transcript);
                (blockers, card, pose, holes, extras, record, text)
            },
            move |viewport, (mut blockers, card, pose, holes, extras, record, text), window, cx| {
                blockers.paint(window, cx);
                effects::backdrop(window, viewport, pose.focus, &holes, pose.shade);
                if let Some(focus) = pose.focus {
                    effects::lighting(window, cue, focus, &extras, record, pose.ring);
                }
                if let (Some(record), Some(text)) = (record, text) {
                    effects::ribbon(window, cue, record, text);
                }
                if let Some(mut card) = card {
                    card.paint(window, cx);
                }
            },
        )
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .size_full()
    }
}

#[derive(Clone, Copy)]
struct Pose {
    focus: Option<Bounds<Pixels>>,
    card: Option<Point<Pixels>>,
    card_from: Option<Point<Pixels>>,
    shade: f32,
    ring: f32,
    catching_up: bool,
}

#[derive(Default)]
struct Camera {
    key: Option<(u64, Scene)>,
    from: Option<Pose>,
    shown: Option<Pose>,
    last_tick: Option<Instant>,
}

impl Camera {
    fn advance(
        &mut self,
        cue: Frame,
        viewport: Bounds<Pixels>,
        target: Option<Bounds<Pixels>>,
        card: Option<Point<Pixels>>,
    ) -> Pose {
        let seed = Pose {
            focus: Some(effects::centered(viewport.center(), px(39.), px(39.))),
            card: card.map(|p| p + point(px(0.), px(18.))),
            card_from: None,
            shade: 0.83,
            ring: 0.16,
            catching_up: false,
        };
        let shown = self.shown.unwrap_or(seed);
        let key = (cue.serial, cue.scene);
        let changed = self.key != Some(key);
        if changed {
            self.key = Some(key);
            self.from = Some(shown);
        }
        let from = self.from.unwrap_or(seed);
        let now = Instant::now();
        let seconds = self
            .last_tick
            .replace(now)
            .map_or(0.016, |last| now.duration_since(last).as_secs_f32())
            .clamp(0.010, 0.050);
        let time = cue.focus();
        let mut catching_up = false;
        let focus = target
            .map(|target| {
                if !cue.moving {
                    return target;
                }
                if time < 1. {
                    return blend_rect(from.focus.unwrap_or(target), target, time)
                        .intersect(&viewport);
                }
                let previous = shown.focus.unwrap_or(target);
                let difference = (previous.left() - target.left()).abs()
                    + (previous.top() - target.top()).abs()
                    + (previous.right() - target.right()).abs()
                    + (previous.bottom() - target.bottom()).abs();
                if difference <= px(2.) {
                    return target;
                }
                catching_up = true;
                blend_rect(previous, target, 1. - (-seconds / 0.038).exp()).intersect(&viewport)
            })
            .filter(|bounds| bounds.size.width > px(0.) && bounds.size.height > px(0.));
        let (shade, ring) = match cue.scene {
            Scene::Welcome => (0.66, 0.79),
            Scene::Finish => (0.14, 0.0),
            Scene::Exit => (0., 0.),
            _ => (0.59, 1.),
        };
        Pose {
            focus,
            card,
            card_from: from.card,
            shade: motion::lerp(from.shade, shade, time),
            ring: motion::lerp(from.ring, ring, time),
            catching_up,
        }
    }
}

fn blend_point(from: Point<Pixels>, to: Point<Pixels>, t: f32) -> Point<Pixels> {
    point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t)
}

fn blend_rect(from: Bounds<Pixels>, to: Bounds<Pixels>, t: f32) -> Bounds<Pixels> {
    Bounds::from_corners(
        blend_point(from.origin, to.origin, t),
        blend_point(from.bottom_right(), to.bottom_right(), t),
    )
}

fn slide_card(
    goal: Bounds<Pixels>,
    previous: Option<Point<Pixels>>,
    time: f32,
    viewport: Bounds<Pixels>,
    protected: &[Bounds<Pixels>],
    centered: bool,
) -> Point<Pixels> {
    if time >= 1. {
        return goal.origin;
    }
    let area = viewport.inset(MARGIN);
    let start = previous.unwrap_or(goal.origin + point(px(0.), px(18.)));
    let start = point(
        start.x.max(area.left()).min(area.right() - goal.size.width),
        start
            .y
            .max(area.top())
            .min(area.bottom() - goal.size.height),
    );
    let mut safe_start = goal.origin;
    for i in 1..=14 {
        let p = blend_point(goal.origin, start, i as f32 / 14.);
        if !centered
            && protected
                .iter()
                .any(|target| Bounds::new(p, goal.size).intersects(target))
        {
            break;
        }
        safe_start = p;
    }
    blend_point(safe_start, goal.origin, time)
}

fn visible_target(target: Target, viewport: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
    let clip = target.clip.intersect(&viewport);
    let visible = target.bounds.intersect(&clip);
    (visible.size.height >= px(10.) && visible.size.width >= px(10.))
        .then(|| visible.dilate(px(4.)).intersect(&clip))
}

fn scroll_to_target(scroll: &ScrollHandle, target: Bounds<Pixels>) -> Option<Point<Pixels>> {
    let viewport = scroll.bounds();
    if viewport.size.height < px(32.) {
        return None;
    }
    let margin = px(8.);
    let top = target.top() - margin;
    let height = (target.size.height + margin * 2.).min(viewport.size.height);
    let offset = scroll.offset();
    let y = if top < viewport.top() {
        offset.y + viewport.top() - top
    } else {
        offset.y - (top + height - viewport.bottom()).max(px(0.))
    }
    .min(px(0.));
    (y != offset.y).then_some(point(offset.x, y))
}

fn place_card(
    viewport: Bounds<Pixels>,
    target: Bounds<Pixels>,
    protected: &[Bounds<Pixels>],
    card: Size<Pixels>,
    to_the_right: bool,
) -> Option<Point<Pixels>> {
    let area = viewport.inset(MARGIN);
    let max_x = area.right() - card.width;
    let max_y = area.bottom() - card.height;
    if max_x < area.left() || max_y < area.top() {
        return None;
    }
    let x = target.left().clamp(area.left(), max_x);
    let y = target.top().clamp(area.top(), max_y);
    let x_end = (target.right() - card.width).clamp(area.left(), max_x);
    let y_end = (target.bottom() - card.height).clamp(area.top(), max_y);
    let top = protected
        .iter()
        .map(Bounds::top)
        .fold(target.top(), Pixels::min);
    let bottom = protected
        .iter()
        .map(Bounds::bottom)
        .fold(target.bottom(), Pixels::max);
    let sideways = [
        point(target.right() + GAP, y),
        point(target.right() + GAP, y_end),
    ];
    let below = [
        point(x, target.bottom() + GAP),
        point(x_end, target.bottom() + GAP),
    ];
    let [first, second] = if to_the_right {
        [sideways, below]
    } else {
        [below, sideways]
    };
    [
        first[0],
        first[1],
        second[0],
        second[1],
        point(target.left() - card.width - GAP, y),
        point(target.left() - card.width - GAP, y_end),
        point(x, target.top() - card.height - GAP),
        point(x_end, target.top() - card.height - GAP),
        point(x, bottom + GAP),
        point(x_end, bottom + GAP),
        point(x, top - card.height - GAP),
        point(x_end, top - card.height - GAP),
    ]
    .into_iter()
    .find(|origin| {
        let bounds = Bounds::new(*origin, card);
        origin.x >= area.left()
            && origin.x <= max_x
            && origin.y >= area.top()
            && origin.y <= max_y
            && !protected.iter().any(|target| bounds.intersects(target))
    })
}

fn subtract_holes(
    viewport: Bounds<Pixels>,
    holes: impl IntoIterator<Item = Bounds<Pixels>>,
) -> Vec<Bounds<Pixels>> {
    let mut shaded = vec![viewport];
    for hole in holes {
        shaded = shaded
            .into_iter()
            .flat_map(|region| {
                let cut = region.intersect(&hole);
                if cut.size.width <= px(0.) || cut.size.height <= px(0.) {
                    return [Some(region), None, None, None];
                }
                [
                    rect(region.left(), region.top(), region.right(), cut.top()),
                    rect(region.left(), cut.bottom(), region.right(), region.bottom()),
                    rect(region.left(), cut.top(), cut.left(), cut.bottom()),
                    rect(cut.right(), cut.top(), region.right(), cut.bottom()),
                ]
            })
            .flatten()
            .collect();
    }
    shaded
}

fn rect(left: Pixels, top: Pixels, right: Pixels, bottom: Pixels) -> Option<Bounds<Pixels>> {
    (right > left && bottom > top)
        .then(|| Bounds::from_corners(point(left, top), point(right, bottom)))
}

fn positioned(bounds: Bounds<Pixels>, origin: Point<Pixels>) -> Div {
    div()
        .absolute()
        .left(bounds.left() - origin.x)
        .top(bounds.top() - origin.y)
        .w(bounds.size.width)
        .h(bounds.size.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    #[test]
    fn card_flips_at_the_edge_and_docks_when_every_side_is_blocked() {
        let window = bounds(0., 0., 1000., 650.);
        let target = bounds(748., 538., 228., 80.);
        let card = size(CARD_WIDTH, px(174.));
        assert_eq!(
            place_card(window, target, &[target], card, false),
            Some(point(px(349.), px(460.)))
        );
        assert!(place_card(window, bounds(81., 85., 838., 480.), &[window], card, false).is_none());
    }

    #[test]
    fn overlapping_cutouts_keep_both_targets_clear() {
        let viewport = bounds(0., 0., 100., 100.);
        let holes = vec![bounds(10., 10., 40., 30.), bounds(30., 25., 60., 50.)];
        let shaded = subtract_holes(viewport, holes.clone());
        for x in 0..100 {
            for y in 0..100 {
                let p = point(px(x as f32 + 0.5), px(y as f32 + 0.5));
                let expected = usize::from(!holes.iter().any(|hole| hole.contains(&p)));
                assert_eq!(
                    shaded.iter().filter(|tile| tile.contains(&p)).count(),
                    expected
                );
            }
        }
    }
}
