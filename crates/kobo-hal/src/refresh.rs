use kobo_abi::{hwtcon, mxcfb};
use kobo_profile::{ColorPanel, DeviceProfile, FramebufferController};
use std::fmt;

/// Which panel-controller interface a device speaks.
///
/// This is not a preference. It is a property of the hardware, read from the
/// profile's framebuffer identifier, and the two interfaces are not
/// interchangeable: they number their waveforms differently, so a plan built
/// for one backend and submitted to the other draws the wrong thing without
/// reporting anything. Keeping the choice in one enum is what stops a waveform
/// constant from being written down anywhere a backend is not also named.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    /// `MediaTek` panel controller, as on the Clara BW.
    Hwtcon,
    /// i.MX6 EPDC, as on the Mark 7 devices.
    Mxcfb,
}

impl Backend {
    /// Resolves the backend from a framebuffer identifier, as reported by the
    /// kernel in `fb_fix_screeninfo.id`.
    ///
    /// An unknown identifier is `None` rather than a guess, in keeping with
    /// the rest of the device model: an unrecognised panel is refused, not
    /// approximated.
    #[must_use]
    pub fn from_framebuffer_id(id: &str) -> Option<Self> {
        match id {
            "hwtcon" => Some(Self::Hwtcon),
            "mxc_epdc_fb" => Some(Self::Mxcfb),
            _ => None,
        }
    }

    /// Resolves the backend a profile describes.
    #[must_use]
    pub fn from_profile(profile: &DeviceProfile) -> Option<Self> {
        Some(Self::from_controller(profile.framebuffer_controller))
    }

    /// The backend that speaks a profile's declared controller interface.
    ///
    /// Total, unlike the string lookups: a profile cannot exist without
    /// declaring its controller.
    #[must_use]
    pub const fn from_controller(controller: FramebufferController) -> Self {
        match controller {
            FramebufferController::Hwtcon => Self::Hwtcon,
            FramebufferController::MxcfbV2 => Self::Mxcfb,
        }
    }

    /// Resolves an intent to the waveform constant verified for this backend.
    ///
    /// Grayscale constants belong to each backend ABI. Color constants never
    /// come from this crate: they are accepted only from a verified HWTCON
    /// [`ColorPanel`] capability.
    ///
    /// # Errors
    ///
    /// Returns [`RefreshError::ColorCapabilityMissing`] when a color intent
    /// has no verified color capability, or
    /// [`RefreshError::ColorBackendUnsupported`] when this backend cannot
    /// perform color updates.
    pub fn waveform(
        self,
        intent: RefreshIntent,
        color: Option<ColorPanel>,
    ) -> Result<u32, RefreshError> {
        Ok(self.lower(intent, false, color)?.waveform)
    }

    fn lower(
        self,
        intent: RefreshIntent,
        requested_full: bool,
        color: Option<ColorPanel>,
    ) -> Result<LoweredRefresh, RefreshError> {
        let grayscale = |waveform| LoweredRefresh {
            waveform,
            full: requested_full,
            flags: 0,
        };
        match (self, intent) {
            (Self::Hwtcon, RefreshIntent::FastFeedback) => Ok(grayscale(hwtcon::WAVEFORM_DU)),
            (Self::Hwtcon, RefreshIntent::TextContent) => Ok(grayscale(hwtcon::WAVEFORM_GL16)),
            (Self::Hwtcon, RefreshIntent::QualityContent) => Ok(grayscale(hwtcon::WAVEFORM_GC16)),
            (Self::Mxcfb, RefreshIntent::FastFeedback) => Ok(grayscale(mxcfb::WAVEFORM_DU)),
            (Self::Mxcfb, RefreshIntent::TextContent) => Ok(grayscale(mxcfb::WAVEFORM_GL16)),
            (Self::Mxcfb, RefreshIntent::QualityContent) => Ok(grayscale(mxcfb::WAVEFORM_GC16)),
            (Self::Mxcfb, RefreshIntent::ColorContent | RefreshIntent::ColorQuality) => {
                Err(RefreshError::ColorBackendUnsupported(Self::Mxcfb))
            }
            (Self::Hwtcon, RefreshIntent::ColorContent) => {
                let color = color.ok_or(RefreshError::ColorCapabilityMissing)?;
                Ok(LoweredRefresh {
                    waveform: color.regal_waveform,
                    full: false,
                    flags: color.cfa_flags,
                })
            }
            (Self::Hwtcon, RefreshIntent::ColorQuality) => {
                let color = color.ok_or(RefreshError::ColorCapabilityMissing)?;
                Ok(LoweredRefresh {
                    waveform: color.clean_waveform,
                    full: true,
                    flags: color.cfa_flags,
                })
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LoweredRefresh {
    waveform: u32,
    full: bool,
    flags: u32,
}

/// Why a refresh intent could not be lowered to a controller request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshError {
    ColorCapabilityMissing,
    ColorBackendUnsupported(Backend),
}

impl fmt::Display for RefreshError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ColorCapabilityMissing => {
                formatter.write_str("color refresh requires verified panel capability")
            }
            Self::ColorBackendUnsupported(backend) => {
                write!(formatter, "color refresh is unsupported on {backend:?}")
            }
        }
    }
}

impl std::error::Error for RefreshError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    #[must_use]
    pub fn clipped(self, screen_width: u32, screen_height: u32) -> Option<Self> {
        let right = self.x.saturating_add(self.width).min(screen_width);
        let bottom = self.y.saturating_add(self.height).min(screen_height);
        if self.x >= right || self.y >= bottom {
            return None;
        }
        Some(Self {
            x: self.x,
            y: self.y,
            width: right - self.x,
            height: bottom - self.y,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshIntent {
    /// A change that is purely black and white.
    ///
    /// `DU` is two-level by construction: it drives every pixel to black or to
    /// white and nothing between. That makes it the fastest waveform and the
    /// wrong one for anything with grey in it, because the panel has no way to
    /// represent the middle and smears what it cannot show.
    FastFeedback,
    /// A change containing grey: antialiased text, rules, images.
    ///
    /// `GL16` resolves sixteen levels without the black-white-black flash of a
    /// full update, which is what makes it usable for text that changes often.
    TextContent,
    /// A complete replacement that also clears accumulated ghosting.
    QualityContent,
    /// A partial chromatic update using the panel's normal color waveform.
    ColorContent,
    /// A full chromatic update that cleans color residue.
    ColorQuality,
}

/// A clipped region and what is to be done to it.
///
/// The plan deliberately carries the *intent* and not a waveform number.
/// Callers describe the change they made; only the display session, which
/// knows the profile, turns that into a number for a particular panel
/// controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefreshPlan {
    pub region: Rect,
    pub intent: RefreshIntent,
    pub full: bool,
}

impl RefreshPlan {
    #[must_use]
    pub fn new(
        region: Rect,
        intent: RefreshIntent,
        full: bool,
        screen_width: u32,
        screen_height: u32,
    ) -> Option<Self> {
        Some(Self {
            region: region.clipped(screen_width, screen_height)?,
            intent,
            full,
        })
    }

    /// The waveform number `backend` uses for this plan.
    ///
    /// # Errors
    ///
    /// Returns an error when the plan requests color without a verified color
    /// capability or on a backend that cannot perform color updates.
    pub fn waveform(
        self,
        backend: Backend,
        color: Option<ColorPanel>,
    ) -> Result<u32, RefreshError> {
        backend.waveform(self.intent, color)
    }

    /// Lowers this plan to a `MediaTek` HWTCON update request.
    ///
    /// # Errors
    ///
    /// Returns [`RefreshError::ColorCapabilityMissing`] when the plan requests
    /// color without a verified color capability.
    pub fn hwtcon_update_data(
        self,
        marker: u32,
        color: Option<ColorPanel>,
    ) -> Result<hwtcon::HwtconUpdateData, RefreshError> {
        let lowered = Backend::Hwtcon.lower(self.intent, self.full, color)?;
        Ok(hwtcon::HwtconUpdateData {
            update_region: hwtcon::HwtconRect {
                top: self.region.y,
                left: self.region.x,
                width: self.region.width,
                height: self.region.height,
            },
            waveform_mode: lowered.waveform,
            update_mode: if lowered.full {
                hwtcon::UPDATE_MODE_FULL
            } else {
                hwtcon::UPDATE_MODE_PARTIAL
            },
            update_marker: marker,
            flags: lowered.flags,
            dither_mode: 0,
        })
    }

    /// The same plan as an i.MX6 EPDC update.
    ///
    /// `temp` asks the controller for its own ambient reading, which is what
    /// every other consumer of this interface sends and the only value here
    /// that is not simply zero. Dithering is left off and the alternate buffer
    /// unused: updates come from the framebuffer itself.
    ///
    /// # Errors
    ///
    /// Returns [`RefreshError::ColorBackendUnsupported`] when the plan
    /// requests a color update, which the i.MX6 EPDC cannot perform.
    pub fn mxcfb_update_data(
        self,
        marker: u32,
        color: Option<ColorPanel>,
    ) -> Result<mxcfb::MxcfbUpdateData, RefreshError> {
        let lowered = Backend::Mxcfb.lower(self.intent, self.full, color)?;
        Ok(mxcfb::MxcfbUpdateData {
            update_region: mxcfb::MxcfbRect {
                top: self.region.y,
                left: self.region.x,
                width: self.region.width,
                height: self.region.height,
            },
            waveform_mode: lowered.waveform,
            update_mode: if lowered.full {
                mxcfb::UPDATE_MODE_FULL
            } else {
                mxcfb::UPDATE_MODE_PARTIAL
            },
            update_marker: marker,
            temp: mxcfb::TEMP_USE_AMBIENT,
            flags: lowered.flags,
            dither_mode: 0,
            quant_bit: 0,
            alt_buffer_data: mxcfb::MxcfbAltBufferData {
                phys_addr: 0,
                width: 0,
                height: 0,
                alt_update_region: mxcfb::MxcfbRect {
                    top: 0,
                    left: 0,
                    width: 0,
                    height: 0,
                },
            },
        })
    }
}

#[derive(Debug)]
pub struct UpdateMarker {
    next: u32,
}

impl UpdateMarker {
    #[must_use]
    pub fn new(seed: u32) -> Self {
        Self { next: seed.max(1) }
    }

    pub fn take(&mut self) -> u32 {
        let marker = self.next;
        self.next = self.next.wrapping_add(1).max(1);
        marker
    }
}

#[cfg(test)]
mod tests {
    use super::{Backend, Rect, RefreshError, RefreshIntent, RefreshPlan, UpdateMarker};
    use kobo_abi::{hwtcon, mxcfb};
    use kobo_profile::{ChannelField, ColorPanel};

    const COLOR_PANEL: ColorPanel = ColorPanel {
        red: ChannelField {
            offset: 0,
            length: 8,
        },
        green: ChannelField {
            offset: 8,
            length: 8,
        },
        blue: ChannelField {
            offset: 16,
            length: 8,
        },
        transparency: ChannelField {
            offset: 24,
            length: 8,
        },
        clean_waveform: 10,
        regal_waveform: 11,
        cfa_flags: 0x600,
        clean_interval: 4,
    };

    #[test]
    fn clips_regions_before_building_update() {
        let plan = RefreshPlan::new(
            Rect {
                x: 1060,
                y: 1440,
                width: 40,
                height: 40,
            },
            RefreshIntent::QualityContent,
            false,
            1072,
            1448,
        )
        .expect("region intersects screen");
        assert_eq!(plan.region.width, 12);
        assert_eq!(plan.region.height, 8);
        assert_eq!(
            plan.waveform(Backend::Hwtcon, None),
            Ok(hwtcon::WAVEFORM_GC16)
        );
    }

    /// The reason the plan stopped carrying a waveform number. Both panels are
    /// being asked for the same thing, and both are being told a different
    /// number. Had the hwtcon number been sent to an i.MX6 panel, it would have
    /// run `GC4` and reported success.
    #[test]
    fn one_intent_becomes_a_different_number_on_each_backend() {
        let plan = RefreshPlan::new(
            Rect {
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            },
            RefreshIntent::TextContent,
            false,
            1264,
            1680,
        )
        .expect("region intersects screen");

        assert_eq!(
            plan.waveform(Backend::Hwtcon, None),
            Ok(hwtcon::WAVEFORM_GL16)
        );
        assert_eq!(
            plan.waveform(Backend::Mxcfb, None),
            Ok(mxcfb::WAVEFORM_GL16)
        );
        assert_ne!(
            plan.waveform(Backend::Hwtcon, None),
            plan.waveform(Backend::Mxcfb, None)
        );
    }

    #[test]
    fn a_backend_comes_from_the_hardware_and_is_never_guessed() {
        assert_eq!(
            Backend::from_framebuffer_id("hwtcon"),
            Some(Backend::Hwtcon)
        );
        assert_eq!(
            Backend::from_framebuffer_id("mxc_epdc_fb"),
            Some(Backend::Mxcfb)
        );
        assert_eq!(Backend::from_framebuffer_id("mxc_epdc_fb2"), None);
        assert_eq!(Backend::from_framebuffer_id(""), None);
    }

    #[test]
    fn an_mxcfb_update_asks_the_panel_for_its_own_temperature() {
        let plan = RefreshPlan::new(
            Rect {
                x: 8,
                y: 16,
                width: 32,
                height: 48,
            },
            RefreshIntent::QualityContent,
            true,
            1264,
            1680,
        )
        .expect("region intersects screen");
        let update = plan
            .mxcfb_update_data(0x4000_0001, None)
            .expect("grayscale update lowers");

        assert_eq!(update.update_region.left, 8);
        assert_eq!(update.update_region.top, 16);
        assert_eq!(update.waveform_mode, mxcfb::WAVEFORM_GC16);
        assert_eq!(update.update_mode, mxcfb::UPDATE_MODE_FULL);
        assert_eq!(update.update_marker, 0x4000_0001);
        assert_eq!(update.temp, mxcfb::TEMP_USE_AMBIENT);
        assert_eq!(update.flags, 0);
        assert_eq!(update.dither_mode, 0);
        assert_eq!(update.quant_bit, 0);
        assert_eq!(update.alt_buffer_data, mxcfb::MxcfbAltBufferData::default());
    }

    #[test]
    fn color_refreshes_carry_profile_waveforms_flags_and_required_modes() {
        let region = Rect {
            x: 8,
            y: 16,
            width: 32,
            height: 48,
        };
        let grayscale = RefreshPlan::new(region, RefreshIntent::TextContent, false, 1264, 1680)
            .expect("grayscale region")
            .hwtcon_update_data(0x4000_0000, Some(COLOR_PANEL))
            .expect("grayscale update");
        assert_eq!(grayscale.waveform_mode, hwtcon::WAVEFORM_GL16);
        assert_eq!(grayscale.update_mode, hwtcon::UPDATE_MODE_PARTIAL);
        assert_eq!(grayscale.flags, 0);

        let regal = RefreshPlan::new(region, RefreshIntent::ColorContent, true, 1264, 1680)
            .expect("color region")
            .hwtcon_update_data(0x4000_0001, Some(COLOR_PANEL))
            .expect("verified color update");
        assert_eq!(regal.waveform_mode, COLOR_PANEL.regal_waveform);
        assert_eq!(regal.update_mode, hwtcon::UPDATE_MODE_PARTIAL);
        assert_eq!(regal.flags, COLOR_PANEL.cfa_flags);

        let clean = RefreshPlan::new(region, RefreshIntent::ColorQuality, false, 1264, 1680)
            .expect("color region")
            .hwtcon_update_data(0x4000_0002, Some(COLOR_PANEL))
            .expect("verified color clean");
        assert_eq!(clean.waveform_mode, COLOR_PANEL.clean_waveform);
        assert_eq!(clean.update_mode, hwtcon::UPDATE_MODE_FULL);
        assert_eq!(clean.flags, COLOR_PANEL.cfa_flags);
    }

    #[test]
    fn color_refreshes_fail_closed_without_hwtcon_capability() {
        let plan = RefreshPlan::new(
            Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            RefreshIntent::ColorContent,
            false,
            1,
            1,
        )
        .expect("color region");

        assert_eq!(
            plan.hwtcon_update_data(1, None),
            Err(RefreshError::ColorCapabilityMissing)
        );
        assert_eq!(
            plan.mxcfb_update_data(1, Some(COLOR_PANEL)),
            Err(RefreshError::ColorBackendUnsupported(Backend::Mxcfb))
        );
    }

    #[test]
    fn markers_never_emit_zero() {
        let mut markers = UpdateMarker::new(u32::MAX);
        assert_eq!(markers.take(), u32::MAX);
        assert_eq!(markers.take(), 1);
    }
}
