//! Procedural 32x32 RGBA tray icon generator.
//!
//! Renders a high-contrast dual-axis icon:
//! - Outer ring: operational state (TUN blue / SysProxy amber / Running emerald / Unreachable gray)
//! - Center disc: active routing mode (Rule light / Global red / Direct charcoal)

use tray_icon::Icon;

use crate::state::{OperationalState, ProxyMode};

const ICON_SIZE: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconKey {
    pub state: OperationalState,
    pub mode: ProxyMode,
}

pub fn render_icon(key: IconKey) -> Result<Icon, String> {
    let rgba = render_rgba(key, ICON_SIZE);
    Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE).map_err(|e| format!("failed to build icon: {e}"))
}

fn render_rgba(key: IconKey, size: u32) -> Vec<u8> {
    let mut buf = vec![0u8; (size * size * 4) as usize];
    let outer_color = state_color(key.state);
    let inner_color = mode_color(key.mode);
    let border_color = [20u8, 24u8, 30u8];

    let center = (size as f32 - 1.0) / 2.0;
    let outer_r = size as f32 * 0.44;
    let ring_inner_r = size as f32 * 0.38;
    let dot_r = size as f32 * 0.19;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();

            let idx = ((y * size + x) * 4) as usize;
            if dist > outer_r + 0.75 {
                continue;
            }

            let alpha_outer = smoothstep_edge(outer_r, dist);
            let mut rgb = if dist > ring_inner_r {
                lerp_rgb(outer_color, border_color, 0.55)
            } else {
                outer_color
            };

            if let Some(inner) = inner_color
                && dist <= dot_r + 0.75
            {
                let dot_alpha = smoothstep_edge(dot_r, dist);
                rgb = lerp_rgb(rgb, inner, dot_alpha);
            }

            buf[idx] = rgb[0];
            buf[idx + 1] = rgb[1];
            buf[idx + 2] = rgb[2];
            buf[idx + 3] = (alpha_outer * 255.0).round() as u8;
        }
    }
    buf
}

fn state_color(state: OperationalState) -> [u8; 3] {
    match state {
        OperationalState::Tun => [33, 150, 243],          // Blue
        OperationalState::SysProxy => [251, 140, 0],      // Amber
        OperationalState::Running => [46, 204, 113],      // Emerald
        OperationalState::Unreachable => [130, 140, 152], // Slate gray
    }
}

fn mode_color(mode: ProxyMode) -> Option<[u8; 3]> {
    match mode {
        ProxyMode::Rule => Some([244, 247, 250]), // Crisp light center
        ProxyMode::Global => Some([229, 57, 53]), // Red
        ProxyMode::Direct => Some([34, 39, 46]),  // Dark charcoal
        ProxyMode::Unknown => None,
    }
}

fn smoothstep_edge(radius: f32, dist: f32) -> f32 {
    (radius + 0.75 - dist).clamp(0.0, 1.0)
}

fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t).round() as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t).round() as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t).round() as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_rgba_has_expected_dimensions_and_distinct_states() {
        let size = 32u32;
        let tun = render_rgba(
            IconKey {
                state: OperationalState::Tun,
                mode: ProxyMode::Rule,
            },
            size,
        );
        let sys = render_rgba(
            IconKey {
                state: OperationalState::SysProxy,
                mode: ProxyMode::Global,
            },
            size,
        );
        let direct = render_rgba(
            IconKey {
                state: OperationalState::Running,
                mode: ProxyMode::Direct,
            },
            size,
        );
        assert_eq!(tun.len(), (size * size * 4) as usize);
        assert_ne!(tun, sys);
        assert_ne!(sys, direct);
        // Corner pixel should stay transparent.
        assert_eq!(tun[3], 0);
        // Center pixel should be opaque.
        let center_idx = ((16 * size + 16) * 4) as usize;
        assert_eq!(tun[center_idx + 3], 255);
        assert_eq!(
            [sys[center_idx], sys[center_idx + 1], sys[center_idx + 2]],
            [229, 57, 53]
        );
    }
}
