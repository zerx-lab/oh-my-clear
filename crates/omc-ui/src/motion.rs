//! Spring presets (ADR 0011). Views animate only through these `const` springs, never an
//! ad-hoc duration: a spring keeps position and velocity when it is retargeted mid-flight.
//! Every preset is critically damped (no bounce); `gpui_kit::base::spring` settles on the
//! spot while the OS asks for reduced motion.

use std::time::Duration;

use gpui_kit::base::Spring;

/// Panels and sidebars entering: 0.35 s, no bounce.
pub const PANEL: Spring = Spring::new(Duration::from_millis(350)).with_epsilon(PIXEL_EPSILON);

/// Panels and sidebars leaving: 0.7 × [`PANEL`], so dismissal is never slower than
/// presentation.
pub const PANEL_EXIT: Spring = Spring::new(Duration::from_millis(245)).with_epsilon(PIXEL_EPSILON);

/// Settling tolerance for springs driving a length in px: half a pixel is invisible, and
/// the default (0.001) would keep requesting frames long after the motion looks done.
const PIXEL_EPSILON: f32 = 0.5;
