use chromiumoxide_cdp::cdp::browser_protocol::{emulation::SetDeviceMetricsOverrideParams, page};

#[derive(Debug, Clone)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    pub device_scale_factor: Option<f64>,
    pub emulating_mobile: bool,
    pub is_landscape: bool,
    pub has_touch: bool,
}

impl Default for Viewport {
    fn default() -> Self {
        Viewport {
            width: 800,
            height: 600,
            device_scale_factor: None,
            emulating_mobile: false,
            is_landscape: false,
            has_touch: false,
        }
    }
}

impl From<Viewport> for SetDeviceMetricsOverrideParams {
    fn from(value: Viewport) -> Self {
        SetDeviceMetricsOverrideParams::new(
            value.width,
            value.height,
            value.device_scale_factor.unwrap_or(1.),
            value.emulating_mobile,
        )
    }
}

impl From<Viewport> for page::Viewport {
    fn from(value: Viewport) -> Self {
        page::Viewport::builder()
            .x(0)
            .y(0)
            .width(value.width)
            .height(value.height)
            .scale(1.)
            .build()
            .unwrap()
    }
}
