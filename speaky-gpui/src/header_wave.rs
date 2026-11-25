use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    canvas, fill, point, prelude::*, px, rgb, rgba, size, Bounds, Context, HitboxBehavior, Window,
};

use crate::ACCENT;

const ENVELOPE: [f32; 33] = [
    0.05, 0.05, 0.08, 0.13, 0.24, 0.36, 0.52, 0.38, 0.16, 0.18, 0.30, 0.53, 0.78, 0.96, 0.83, 0.60,
    0.34, 0.17, 0.28, 0.50, 0.72, 0.85, 0.63, 0.44, 0.18, 0.16, 0.27, 0.37, 0.26, 0.16, 0.09, 0.05,
    0.05,
];

#[derive(Default)]
pub(crate) struct HeaderWave {
    mic_level: Option<f32>,
    motion: Rc<Cell<Motion>>,
}

impl HeaderWave {
    pub(crate) fn set_level(&mut self, level: Option<f32>) {
        self.mic_level = level;
    }
}

#[derive(Clone, Copy, Default)]
struct Motion {
    previous_frame: Option<Instant>,
    frame_pending: bool,
    time: f32,
    pointer_x: f32,
    hover: f32,
    recording: f32,
    voice: f32,
}

impl Motion {
    fn advance(&mut self, now: Instant, pointer_x: Option<f32>, level: Option<f32>) {
        let dt = self
            .previous_frame
            .replace(now)
            .map_or(1. / 60., |previous| {
                (now - previous).as_secs_f32().min(0.05)
            });
        self.time += dt;

        if let Some(x) = pointer_x {
            if self.hover < 0.01 {
                self.pointer_x = x;
            }
            ease(&mut self.pointer_x, x, dt, 0.046);
        }
        let (hover_target, hover_response) = if pointer_x.is_some() {
            (1., 0.038)
        } else {
            (0., 0.16)
        };
        ease(&mut self.hover, hover_target, dt, hover_response);

        // Average absolute PCM level is small for normal speech. Compress the
        // range so quiet speech still moves the bars and silence rests low.
        let voice = ((level.unwrap_or(0.) - 0.001).max(0.) * 16.).sqrt().min(1.);
        let voice_response = if voice > self.voice { 0.035 } else { 0.15 };
        ease(&mut self.voice, voice, dt, voice_response);
        ease(
            &mut self.recording,
            if level.is_some() { 1. } else { 0. },
            dt,
            0.12,
        );
    }
}

fn ease(value: &mut f32, target: f32, dt: f32, response: f32) {
    *value += (target - *value) * (1. - (-dt / response).exp());
}

impl Render for HeaderWave {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        let motion = self.motion.clone();
        let level = self.mic_level;
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, cx| {
                if !bounds.intersects(&window.content_mask().bounds) {
                    return;
                }
                let pointer = (window.is_window_hovered() && hitbox.is_hovered(window))
                    .then(|| f32::from(window.mouse_position().x - bounds.origin.x));
                let mut frame = motion.get();
                frame.advance(Instant::now(), pointer, level);
                motion.set(frame);

                let width = f32::from(bounds.size.width);
                let height = f32::from(bounds.size.height);
                let mid_y = bounds.origin.y + px(height / 2.);
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.origin.x, mid_y - px(0.5)),
                        size(px(width), px(1.)),
                    ),
                    rgba(0x2858482b),
                ));
                let spacing = (width - 18.) / (ENVELOPE.len() - 1) as f32;
                for (index, envelope) in ENVELOPE.into_iter().enumerate() {
                    let i = index as f32;
                    let x = 9. + i * spacing;
                    let t = frame.time;
                    let ripple = 0.83
                        + 0.16 * (2.1 * t - 0.46 * i).sin()
                        + 0.11 * (1.37 * t + 0.78 * i).sin()
                        + 0.07 * (3.07 * t - 1.34 * i).sin();
                    let idle = envelope * ripple * 0.81;
                    let voice = frame.voice * (0.17 + 0.83 * envelope) * (0.65 + 0.35 * ripple);
                    let drive = idle + (voice - idle) * frame.recording;

                    let near = (1. - (x - frame.pointer_x).abs() / 24.).clamp(0., 1.);
                    let hover = near * near * (3. - 2. * near) * frame.hover;
                    let magnitude = drive + (0.68 - 0.33 * drive) * hover;
                    let bar_height = (2.8 + (height - 5.) * magnitude).min(height - 2.);

                    let mut tint = rgb(0x11846a);
                    tint.a = hover;
                    let mut color = rgb(ACCENT).blend(tint);
                    color.a = (0.27 + 0.51 * envelope + 0.23 * voice + 0.56 * hover).min(1.);
                    let mut bar = fill(
                        Bounds::new(
                            point(bounds.origin.x + px(x - 1.), mid_y - px(bar_height / 2.)),
                            size(px(2.), px(bar_height)),
                        ),
                        color,
                    );
                    bar.corner_radii = 1.0.into();
                    window.paint_quad(bar);
                }
                // Schedule only while painted, and use fewer frames for idle motion.
                if !frame.frame_pending {
                    frame.frame_pending = true;
                    motion.set(frame);
                    let interval = if frame.hover > 0.01 || level.is_some() {
                        16
                    } else {
                        32
                    };
                    let timer = cx
                        .background_executor()
                        .timer(Duration::from_millis(interval));
                    cx.spawn(async move |cx| {
                        timer.await;
                        let mut frame = motion.get();
                        frame.frame_pending = false;
                        motion.set(frame);
                        let _ = view.update(cx, |_, cx| cx.notify());
                    })
                    .detach();
                }
            },
        )
        .w(px(174.))
        .h(px(40.))
        .flex_none()
    }
}
