//! OKLCH → `Hsla` conversion for dial-authored colours (ADR 0011: authored in OKLCH,
//! converted once when the theme is applied, never in `render`). Björn Ottosson's
//! reference matrices (<https://bottosson.github.io/posts/oklab/>).

use gpui_kit::{Hsla, Rgba};

/// Converts OKLCH (`l` 0–1, `c` chroma ≈0–0.37, `h` degrees) to an opaque sRGB colour,
/// clamping out-of-gamut channels.
pub(crate) fn oklch(lightness: f32, chroma: f32, hue: f32) -> Hsla {
    let (sin, cos) = hue.to_radians().sin_cos();
    let (green_red, blue_yellow) = (chroma * cos, chroma * sin);

    // OKLab → LMS cone responses (cubed back from the perceptual cube root).
    let long = (lightness + 0.396_337_78 * green_red + 0.215_803_76 * blue_yellow).powi(3);
    let medium = (lightness - 0.105_561_346 * green_red - 0.063_854_17 * blue_yellow).powi(3);
    let short = (lightness - 0.089_484_18 * green_red - 1.291_485_5 * blue_yellow).powi(3);

    // LMS → linear sRGB.
    let red = 4.076_741_7 * long - 3.307_711_6 * medium + 0.230_969_94 * short;
    let green = -1.268_438 * long + 2.609_757_4 * medium - 0.341_319_38 * short;
    let blue = -0.004_196_086_3 * long - 0.703_418_6 * medium + 1.707_614_7 * short;

    Hsla::from(Rgba {
        r: gamma(red),
        g: gamma(green),
        b: gamma(blue),
        a: 1.,
    })
}

/// Linear light → sRGB transfer, clamped to the displayable range.
fn gamma(linear: f32) -> f32 {
    let x = linear.clamp(0., 1.);
    if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1. / 2.4) - 0.055
    }
}

/// WCAG 2 relative luminance.
#[cfg(test)]
pub(crate) fn luminance(color: Hsla) -> f32 {
    fn linear(channel: f32) -> f32 {
        if channel <= 0.040_45 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }
    let rgb = color.to_rgb();
    0.2126 * linear(rgb.r) + 0.7152 * linear(rgb.g) + 0.0722 * linear(rgb.b)
}

/// WCAG 2 contrast ratio between two opaque colours (1–21).
#[cfg(test)]
pub(crate) fn contrast(a: Hsla, b: Hsla) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn oklch_matches_reference_srgb_points() {
        let white = oklch(1., 0., 0.).to_rgb();
        assert!(
            close(white.r, 1.) && close(white.g, 1.) && close(white.b, 1.),
            "L=1 must be white: {white:?}"
        );
        let black = oklch(0., 0., 0.).to_rgb();
        assert!(
            close(black.r, 0.) && close(black.g, 0.) && close(black.b, 0.),
            "L=0 must be black: {black:?}"
        );
        // CSS Color 4: oklch(62.8% 0.2577 29.23) == #ff0000.
        let red = oklch(0.628, 0.2577, 29.23).to_rgb();
        assert!(
            close(red.r, 1.) && close(red.g, 0.) && close(red.b, 0.),
            "reference red: {red:?}"
        );
    }

    #[test]
    fn contrast_spans_one_to_twenty_one() {
        let white = oklch(1., 0., 0.);
        let black = oklch(0., 0., 0.);
        assert!(
            (contrast(white, black) - 21.).abs() < 0.1,
            "black on white is 21:1"
        );
        assert!(
            (contrast(white, white) - 1.).abs() < 0.001,
            "a colour on itself is 1:1"
        );
    }
}
