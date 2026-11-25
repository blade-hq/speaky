use crate::{
    d2d::{fill, rect, Graphics},
    overlay::{clock, Layer},
};
use windows_sys::Win32::Foundation::HWND;

pub fn create(owner: HWND, graphics: &Graphics) -> Result<Layer, String> {
    Layer::new(owner, graphics, 180, 40, "win7-floating-position")
}

pub fn render(
    layer: &mut Layer,
    graphics: &Graphics,
    recording: Option<(f32, u64)>,
    elapsed: f32,
    status: &str,
) -> windows::core::Result<()> {
    layer.surface.draw(layer.hwnd, |target| {
        fill(target, rect(0., 0., 180., 40.), 20., 0x25334a, 0.94)?;
        if let Some((level, seconds)) = recording {
            let level = (level * 7.).clamp(0., 1.).sqrt();
            for i in 0..6 {
                let height =
                    4. + 19. * level * (0.65 + 0.35 * (elapsed * 12. + i as f32 * 1.2).sin());
                let x = 45. + i as f32 * 6.;
                fill(
                    target,
                    rect(x, 20. - height / 2., x + 4., 20. + height / 2.),
                    2.,
                    0x81bfff,
                    1.,
                )?;
            }
            graphics.text(target, "✕", rect(14., 10., 40., 32.))?;
            graphics.text(target, &clock(seconds), rect(92., 10., 142., 32.))?;
            graphics.text(target, "✓", rect(150., 10., 176., 32.))?;
        } else {
            graphics.text(target, status, rect(16., 10., 117., 32.))?;
        }
        Ok(())
    })
}
