//! Reversible framebuffer region access.
//!
//! Regions are read and written with positional file I/O rather than a memory
//! mapping, so no unsafe code is required and every access is bounds checked
//! against both the visible screen and the reported framebuffer length.
//!
//! Reading is always available. Writing pixels requires the non-default
//! `device-write` feature, so a default build has no callable pixel-write path.

use crate::refresh::Rect;
use kobo_pixels::PicturePixelsRef;
use kobo_profile::{ChannelField, ColorPanel};
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

/// Bytes per pixel required by every supported Kobo surface.
pub const SUPPORTED_BYTES_PER_PIXEL: usize = 4;

/// Byte index of the alpha channel inside a supported pixel.
///
/// The verified Clara BW surface reports red/green/blue/alpha at bit offsets
/// 0/8/16/24, which on this little-endian device places alpha in the last byte.
pub const ALPHA_BYTE_INDEX: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceGeometry {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bits_per_pixel: u32,
    pub memory_length: u64,
}

#[derive(Debug)]
pub enum SurfaceError {
    UnsupportedPixelFormat,
    InconsistentGeometry,
    RegionOutsideScreen,
    RegionOutsideMemory,
    RegionMismatch,
    Io(io::Error),
}

impl std::fmt::Display for SurfaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedPixelFormat => {
                formatter.write_str("surface pixel or channel format is unsupported")
            }
            Self::InconsistentGeometry => {
                formatter.write_str("surface stride or length is inconsistent with its resolution")
            }
            Self::RegionOutsideScreen => formatter.write_str("region falls outside the screen"),
            Self::RegionOutsideMemory => {
                formatter.write_str("region falls outside the mapped framebuffer length")
            }
            Self::RegionMismatch => {
                formatter.write_str("snapshot does not describe the requested region")
            }
            Self::Io(error) => write!(formatter, "framebuffer io: {error}"),
        }
    }
}

impl std::error::Error for SurfaceError {}

impl From<io::Error> for SurfaceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChannelOffsets {
    red: u32,
    green: u32,
    blue: u32,
    transparency: u32,
}

impl ChannelOffsets {
    const LEGACY_GRAYSCALE: Self = Self {
        red: 0,
        green: 8,
        blue: 16,
        transparency: 24,
    };

    fn from_color(color: ColorPanel) -> Result<Self, SurfaceError> {
        let fields = [color.red, color.green, color.blue, color.transparency];
        let mut occupied = 0_u32;
        for field in fields {
            let mask = channel_mask(field).ok_or(SurfaceError::UnsupportedPixelFormat)?;
            if occupied & mask != 0 {
                return Err(SurfaceError::UnsupportedPixelFormat);
            }
            occupied |= mask;
        }
        Ok(Self {
            red: u32::from(color.red.offset),
            green: u32::from(color.green.offset),
            blue: u32::from(color.blue.offset),
            transparency: u32::from(color.transparency.offset),
        })
    }

    fn pack(self, red: u8, green: u8, blue: u8) -> [u8; SUPPORTED_BYTES_PER_PIXEL] {
        let word = (u32::from(red) << self.red)
            | (u32::from(green) << self.green)
            | (u32::from(blue) << self.blue)
            | (u32::from(u8::MAX) << self.transparency);
        word.to_le_bytes()
    }

    fn rgb_mask(self) -> u32 {
        (u32::from(u8::MAX) << self.red)
            | (u32::from(u8::MAX) << self.green)
            | (u32::from(u8::MAX) << self.blue)
    }
}

fn channel_mask(field: ChannelField) -> Option<u32> {
    if field.length != 8 || u16::from(field.offset) + u16::from(field.length) > 32 {
        return None;
    }
    Some(u32::from(u8::MAX) << field.offset)
}

/// A validated placement of one region inside one surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionPlacement {
    region: Rect,
    row_bytes: usize,
    first_row_offset: u64,
    stride: u64,
}

impl RegionPlacement {
    /// Validates `region` against `geometry`.
    ///
    /// # Errors
    ///
    /// Returns an error when the pixel format is unsupported, the geometry is
    /// self-inconsistent, or the region leaves the screen or the framebuffer.
    pub fn new(geometry: SurfaceGeometry, region: Rect) -> Result<Self, SurfaceError> {
        let bytes_per_pixel = usize::try_from(geometry.bits_per_pixel)
            .ok()
            .filter(|bits| *bits == SUPPORTED_BYTES_PER_PIXEL * 8)
            .map(|_| SUPPORTED_BYTES_PER_PIXEL)
            .ok_or(SurfaceError::UnsupportedPixelFormat)?;

        let stride = u64::from(geometry.stride);
        let visible_row_bytes = u64::from(geometry.width)
            .checked_mul(bytes_per_pixel as u64)
            .ok_or(SurfaceError::InconsistentGeometry)?;
        let required_length = stride
            .checked_mul(u64::from(geometry.height))
            .ok_or(SurfaceError::InconsistentGeometry)?;
        if geometry.width == 0
            || geometry.height == 0
            || stride < visible_row_bytes
            || geometry.memory_length < required_length
        {
            return Err(SurfaceError::InconsistentGeometry);
        }

        if region.width == 0 || region.height == 0 {
            return Err(SurfaceError::RegionOutsideScreen);
        }
        let right = region
            .x
            .checked_add(region.width)
            .ok_or(SurfaceError::RegionOutsideScreen)?;
        let bottom = region
            .y
            .checked_add(region.height)
            .ok_or(SurfaceError::RegionOutsideScreen)?;
        if right > geometry.width || bottom > geometry.height {
            return Err(SurfaceError::RegionOutsideScreen);
        }

        let row_bytes = usize::try_from(region.width)
            .ok()
            .and_then(|width| width.checked_mul(bytes_per_pixel))
            .ok_or(SurfaceError::RegionOutsideMemory)?;
        let first_row_offset = u64::from(region.y)
            .checked_mul(stride)
            .and_then(|row| {
                row.checked_add(u64::from(region.x).checked_mul(bytes_per_pixel as u64)?)
            })
            .ok_or(SurfaceError::RegionOutsideMemory)?;
        let last_row_end = u64::from(bottom - 1)
            .checked_mul(stride)
            .and_then(|row| row.checked_add(u64::from(right).checked_mul(bytes_per_pixel as u64)?))
            .ok_or(SurfaceError::RegionOutsideMemory)?;
        if last_row_end > geometry.memory_length {
            return Err(SurfaceError::RegionOutsideMemory);
        }

        Ok(Self {
            region,
            row_bytes,
            first_row_offset,
            stride,
        })
    }

    #[must_use]
    pub fn region(&self) -> Rect {
        self.region
    }

    #[must_use]
    pub fn row_bytes(&self) -> usize {
        self.row_bytes
    }

    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.row_bytes.saturating_mul(self.region.height as usize)
    }

    fn row_offset(&self, row: u32) -> Option<u64> {
        u64::from(row)
            .checked_mul(self.stride)
            .and_then(|delta| self.first_row_offset.checked_add(delta))
    }
}

/// The exact bytes of one framebuffer region, sufficient to restore it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegionSnapshot {
    placement: RegionPlacement,
    pixels: Vec<u8>,
    channels: ChannelOffsets,
}

impl RegionSnapshot {
    #[must_use]
    pub fn placement(&self) -> RegionPlacement {
        self.placement
    }

    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Returns the same region with its measured red, green, and blue fields
    /// inverted and its transparency field preserved. This is the reversible
    /// test pattern used by smoke tests.
    #[must_use]
    pub fn inverted_rgb(&self) -> Self {
        let mut pixels = self.pixels.clone();
        let rgb_mask = self.channels.rgb_mask();
        for pixel in pixels.chunks_exact_mut(SUPPORTED_BYTES_PER_PIXEL) {
            let word = u32::from_le_bytes([pixel[0], pixel[1], pixel[2], pixel[3]]) ^ rgb_mask;
            pixel.copy_from_slice(&word.to_le_bytes());
        }
        Self {
            placement: self.placement,
            pixels,
            channels: self.channels,
        }
    }

    /// Returns whether the two snapshots cover the same region byte for byte.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        self.placement == other.placement && self.pixels == other.pixels
    }

    /// Builds a writable region from typed rendered pixels.
    ///
    /// Gray pixels are expanded to equal red, green, and blue values. RGB
    /// pixels require measured channel fields; grayscale without a color
    /// capability preserves the verified legacy channel order. Snapshot rows
    /// contain only region pixels, while placement retains the framebuffer
    /// stride used by reads and writes.
    ///
    /// # Errors
    ///
    /// Returns an error when the geometry or region is invalid, the typed byte
    /// length does not exactly describe the region, RGB pixels have no color
    /// mapping, or any supplied channel field is unsupported.
    pub fn from_pixels(
        geometry: SurfaceGeometry,
        region: Rect,
        source: PicturePixelsRef<'_>,
        color: Option<ColorPanel>,
    ) -> Result<Self, SurfaceError> {
        let placement = RegionPlacement::new(geometry, region)?;
        let pixel_count = usize::try_from(region.width)
            .ok()
            .and_then(|width| {
                usize::try_from(region.height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .ok_or(SurfaceError::RegionMismatch)?;
        let (source_bytes, source_bytes_per_pixel) = match source {
            PicturePixelsRef::Gray8(bytes) => (bytes, 1),
            PicturePixelsRef::Rgb8(bytes) => (bytes, 3),
        };
        let expected_source_bytes = pixel_count
            .checked_mul(source_bytes_per_pixel)
            .ok_or(SurfaceError::RegionMismatch)?;
        if source_bytes.len() != expected_source_bytes {
            return Err(SurfaceError::RegionMismatch);
        }

        let channels = match color {
            Some(color) => ChannelOffsets::from_color(color)?,
            None if matches!(source, PicturePixelsRef::Gray8(_)) => {
                ChannelOffsets::LEGACY_GRAYSCALE
            }
            None => return Err(SurfaceError::UnsupportedPixelFormat),
        };
        let mut pixels = vec![0_u8; placement.total_bytes()];
        match source {
            PicturePixelsRef::Gray8(gray) => {
                for (target, tone) in pixels
                    .chunks_exact_mut(SUPPORTED_BYTES_PER_PIXEL)
                    .zip(gray.iter().copied())
                {
                    target.copy_from_slice(&channels.pack(tone, tone, tone));
                }
            }
            PicturePixelsRef::Rgb8(rgb) => {
                for (target, source) in pixels
                    .chunks_exact_mut(SUPPORTED_BYTES_PER_PIXEL)
                    .zip(rgb.chunks_exact(3))
                {
                    target.copy_from_slice(&channels.pack(source[0], source[1], source[2]));
                }
            }
        }
        Ok(Self {
            placement,
            pixels,
            channels,
        })
    }
}

/// Reads the exact bytes of `region`.
///
/// # Errors
///
/// Returns an error when the region is invalid or the read fails.
pub fn read_region(
    framebuffer: &File,
    geometry: SurfaceGeometry,
    region: Rect,
) -> Result<RegionSnapshot, SurfaceError> {
    let placement = RegionPlacement::new(geometry, region)?;
    let mut pixels = vec![0_u8; placement.total_bytes()];
    for row in 0..placement.region.height {
        let offset = placement
            .row_offset(row)
            .ok_or(SurfaceError::RegionOutsideMemory)?;
        let start = (row as usize).saturating_mul(placement.row_bytes);
        let end = start.saturating_add(placement.row_bytes);
        let slice = pixels
            .get_mut(start..end)
            .ok_or(SurfaceError::RegionOutsideMemory)?;
        framebuffer.read_exact_at(slice, offset)?;
    }
    Ok(RegionSnapshot {
        placement,
        pixels,
        channels: ChannelOffsets::LEGACY_GRAYSCALE,
    })
}

/// Writes `snapshot` back to the region it came from.
///
/// The snapshot carries its own validated placement, so this cannot address any
/// region other than the one that was read.
///
/// # Errors
///
/// Returns an error when the snapshot does not describe `region` or the write
/// fails.
#[cfg(feature = "device-write")]
pub fn write_region(
    framebuffer: &File,
    geometry: SurfaceGeometry,
    snapshot: &RegionSnapshot,
) -> Result<(), SurfaceError> {
    let placement = RegionPlacement::new(geometry, snapshot.placement.region)?;
    if placement != snapshot.placement || snapshot.pixels.len() != placement.total_bytes() {
        return Err(SurfaceError::RegionMismatch);
    }
    for row in 0..placement.region.height {
        let offset = placement
            .row_offset(row)
            .ok_or(SurfaceError::RegionOutsideMemory)?;
        let start = (row as usize).saturating_mul(placement.row_bytes);
        let end = start.saturating_add(placement.row_bytes);
        let slice = snapshot
            .pixels
            .get(start..end)
            .ok_or(SurfaceError::RegionOutsideMemory)?;
        framebuffer.write_all_at(slice, offset)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        read_region, ChannelOffsets, RegionPlacement, RegionSnapshot, SurfaceError,
        SurfaceGeometry, ALPHA_BYTE_INDEX, SUPPORTED_BYTES_PER_PIXEL,
    };
    use crate::refresh::Rect;
    use kobo_pixels::PicturePixelsRef;
    use kobo_profile::{ChannelField, ColorPanel};
    const CLARA: SurfaceGeometry = SurfaceGeometry {
        width: 1072,
        height: 1448,
        stride: 4288,
        bits_per_pixel: 32,
        memory_length: 6_243_328,
    };

    const BGRA_PANEL: ColorPanel = ColorPanel {
        red: ChannelField {
            offset: 16,
            length: 8,
        },
        green: ChannelField {
            offset: 8,
            length: 8,
        },
        blue: ChannelField {
            offset: 0,
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

    const TRANSPARENCY_FIRST_PANEL: ColorPanel = ColorPanel {
        red: ChannelField {
            offset: 8,
            length: 8,
        },
        green: ChannelField {
            offset: 16,
            length: 8,
        },
        blue: ChannelField {
            offset: 24,
            length: 8,
        },
        transparency: ChannelField {
            offset: 0,
            length: 8,
        },
        clean_waveform: 10,
        regal_waveform: 11,
        cfa_flags: 0x600,
        clean_interval: 4,
    };

    #[test]
    fn from_rgb_packs_offsets_and_excludes_stride_padding() {
        let geometry = SurfaceGeometry {
            width: 2,
            height: 1,
            stride: 12,
            bits_per_pixel: 32,
            memory_length: 12,
        };
        let region = Rect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };

        let snapshot = RegionSnapshot::from_pixels(
            geometry,
            region,
            PicturePixelsRef::Rgb8(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]),
            Some(BGRA_PANEL),
        )
        .expect("valid RGB pixels and measured channels");

        assert_eq!(
            snapshot.pixels(),
            &[0x33, 0x22, 0x11, 0xff, 0x66, 0x55, 0x44, 0xff]
        );
        assert_eq!(snapshot.placement().row_bytes(), 8);
    }

    #[test]
    fn from_rgb_places_opaque_transparency_at_measured_offset() {
        let geometry = SurfaceGeometry {
            width: 1,
            height: 1,
            stride: 4,
            bits_per_pixel: 32,
            memory_length: 4,
        };
        let region = Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };

        let snapshot = RegionSnapshot::from_pixels(
            geometry,
            region,
            PicturePixelsRef::Rgb8(&[0x11, 0x22, 0x33]),
            Some(TRANSPARENCY_FIRST_PANEL),
        )
        .expect("valid RGB pixels and measured channels");

        assert_eq!(snapshot.pixels(), &[0xff, 0x11, 0x22, 0x33]);
    }

    #[test]
    fn from_pixels_inversion_preserves_measured_transparency() {
        let geometry = SurfaceGeometry {
            width: 1,
            height: 1,
            stride: 4,
            bits_per_pixel: 32,
            memory_length: 4,
        };
        let region = Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        let snapshot = RegionSnapshot::from_pixels(
            geometry,
            region,
            PicturePixelsRef::Rgb8(&[0x11, 0x22, 0x33]),
            Some(TRANSPARENCY_FIRST_PANEL),
        )
        .expect("valid RGB pixels and measured channels");

        let inverted = snapshot.inverted_rgb();

        assert_eq!(inverted.pixels(), &[0xff, 0xee, 0xdd, 0xcc]);
        assert!(inverted.inverted_rgb().matches(&snapshot));
    }

    #[test]
    fn from_rgb_rejects_missing_or_invalid_color_mapping() {
        let geometry = SurfaceGeometry {
            width: 1,
            height: 1,
            stride: 4,
            bits_per_pixel: 32,
            memory_length: 4,
        };
        let region = Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        assert!(matches!(
            RegionSnapshot::from_pixels(geometry, region, PicturePixelsRef::Rgb8(&[1, 2, 3]), None,),
            Err(SurfaceError::UnsupportedPixelFormat)
        ));

        let mut short_channel = BGRA_PANEL;
        short_channel.red.length = 7;
        let mut outside_word = BGRA_PANEL;
        outside_word.transparency.offset = 25;
        let mut overlapping = BGRA_PANEL;
        overlapping.red.offset = overlapping.green.offset;
        for color in [short_channel, outside_word, overlapping] {
            assert!(matches!(
                RegionSnapshot::from_pixels(
                    geometry,
                    region,
                    PicturePixelsRef::Rgb8(&[1, 2, 3]),
                    Some(color),
                ),
                Err(SurfaceError::UnsupportedPixelFormat)
            ));
        }
    }

    #[test]
    fn from_rgb_rejects_wrong_typed_length() {
        let region = Rect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        let geometry = SurfaceGeometry {
            width: 2,
            height: 1,
            stride: 8,
            bits_per_pixel: 32,
            memory_length: 8,
        };

        assert!(matches!(
            RegionSnapshot::from_pixels(
                geometry,
                region,
                PicturePixelsRef::Rgb8(&[1, 2, 3, 4, 5]),
                Some(BGRA_PANEL),
            ),
            Err(SurfaceError::RegionMismatch)
        ));
    }

    #[test]
    fn from_pixels_gray8_expands_to_equal_rgb_channels() {
        let geometry = SurfaceGeometry {
            width: 2,
            height: 1,
            stride: 8,
            bits_per_pixel: 32,
            memory_length: 8,
        };
        let region = Rect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };

        let snapshot = RegionSnapshot::from_pixels(
            geometry,
            region,
            PicturePixelsRef::Gray8(&[0x12, 0xab]),
            None,
        )
        .expect("valid grayscale pixels");

        assert_eq!(
            snapshot.pixels(),
            &[0x12, 0x12, 0x12, 0xff, 0xab, 0xab, 0xab, 0xff]
        );
    }

    #[test]
    fn from_pixels_gray8_uses_valid_color_mapping() {
        let geometry = SurfaceGeometry {
            width: 1,
            height: 1,
            stride: 4,
            bits_per_pixel: 32,
            memory_length: 4,
        };
        let region = Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };

        let snapshot = RegionSnapshot::from_pixels(
            geometry,
            region,
            PicturePixelsRef::Gray8(&[0x7f]),
            Some(TRANSPARENCY_FIRST_PANEL),
        )
        .expect("valid grayscale pixels and measured channels");

        assert_eq!(snapshot.pixels(), &[0xff, 0x7f, 0x7f, 0x7f]);
    }

    #[test]
    fn from_pixels_gray8_rejects_wrong_typed_length() {
        let geometry = SurfaceGeometry {
            width: 2,
            height: 1,
            stride: 8,
            bits_per_pixel: 32,
            memory_length: 8,
        };
        let region = Rect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };

        assert!(matches!(
            RegionSnapshot::from_pixels(geometry, region, PicturePixelsRef::Gray8(&[1]), None,),
            Err(SurfaceError::RegionMismatch)
        ));
    }

    #[test]
    fn places_the_verified_smoke_region() {
        let placement = RegionPlacement::new(
            CLARA,
            Rect {
                x: 512,
                y: 704,
                width: 32,
                height: 32,
            },
        )
        .expect("region is inside the screen");
        assert_eq!(placement.row_bytes(), 128);
        assert_eq!(placement.total_bytes(), 4096);
        assert_eq!(placement.row_offset(0), Some(704 * 4288 + 512 * 4));
        assert_eq!(placement.row_offset(31), Some(735 * 4288 + 512 * 4));
    }

    #[test]
    fn rejects_regions_that_leave_the_screen() {
        for region in [
            Rect {
                x: 1072,
                y: 0,
                width: 1,
                height: 1,
            },
            Rect {
                x: 0,
                y: 1448,
                width: 1,
                height: 1,
            },
            Rect {
                x: 1040,
                y: 0,
                width: 64,
                height: 1,
            },
            Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 8,
            },
            Rect {
                x: u32::MAX,
                y: 0,
                width: 8,
                height: 8,
            },
        ] {
            assert!(matches!(
                RegionPlacement::new(CLARA, region),
                Err(SurfaceError::RegionOutsideScreen)
            ));
        }
    }

    #[test]
    fn rejects_unsupported_or_inconsistent_surfaces() {
        let mut geometry = CLARA;
        geometry.bits_per_pixel = 16;
        assert!(matches!(
            RegionPlacement::new(
                geometry,
                Rect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8
                }
            ),
            Err(SurfaceError::UnsupportedPixelFormat)
        ));

        let mut short_stride = CLARA;
        short_stride.stride = 1024;
        assert!(matches!(
            RegionPlacement::new(
                short_stride,
                Rect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8
                }
            ),
            Err(SurfaceError::InconsistentGeometry)
        ));

        let mut short_memory = CLARA;
        short_memory.memory_length = 4288;
        assert!(matches!(
            RegionPlacement::new(
                short_memory,
                Rect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8
                }
            ),
            Err(SurfaceError::InconsistentGeometry)
        ));
    }

    #[test]
    fn inversion_preserves_alpha_and_is_its_own_inverse() {
        let placement = RegionPlacement::new(
            CLARA,
            Rect {
                x: 0,
                y: 0,
                width: 2,
                height: 1,
            },
        )
        .expect("region is inside the screen");
        let snapshot = RegionSnapshot {
            placement,
            pixels: vec![0x10, 0x20, 0x30, 0xff, 0x01, 0x02, 0x03, 0x7f],
            channels: ChannelOffsets::LEGACY_GRAYSCALE,
        };
        let inverted = snapshot.inverted_rgb();
        assert_eq!(
            inverted.pixels(),
            [0xef, 0xdf, 0xcf, 0xff, 0xfe, 0xfd, 0xfc, 0x7f]
        );
        for (index, byte) in inverted.pixels().iter().enumerate() {
            if index % SUPPORTED_BYTES_PER_PIXEL == ALPHA_BYTE_INDEX {
                assert_eq!(*byte, snapshot.pixels()[index]);
            }
        }
        assert!(inverted.inverted_rgb().matches(&snapshot));
        assert!(!inverted.matches(&snapshot));
    }

    #[test]
    fn captured_snapshot_inversion_uses_legacy_channel_layout() {
        let geometry = SurfaceGeometry {
            width: 1,
            height: 1,
            stride: 4,
            bits_per_pixel: 32,
            memory_length: 4,
        };
        let path = std::env::temp_dir().join(format!(
            "kobo-surface-invert-{}-{}.bin",
            std::process::id(),
            line!()
        ));
        std::fs::write(&path, [0x10, 0x20, 0x30, 0x7f]).expect("write fixture");
        let file = std::fs::File::open(&path).expect("open fixture");
        let snapshot = read_region(
            &file,
            geometry,
            Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
        )
        .expect("read region");
        drop(file);
        std::fs::remove_file(&path).expect("remove fixture");

        assert_eq!(snapshot.inverted_rgb().pixels(), &[0xef, 0xdf, 0xcf, 0x7f]);
    }

    #[test]
    fn reads_exact_region_rows_from_a_file() {
        let geometry = SurfaceGeometry {
            width: 4,
            height: 3,
            stride: 24,
            bits_per_pixel: 32,
            memory_length: 72,
        };
        let path = std::env::temp_dir().join(format!(
            "kobo-surface-{}-{}.bin",
            std::process::id(),
            line!()
        ));
        let contents: Vec<u8> = (0..72_u8).collect();
        std::fs::write(&path, &contents).expect("write fixture");
        let file = std::fs::File::open(&path).expect("open fixture");
        let snapshot = read_region(
            &file,
            geometry,
            Rect {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            },
        )
        .expect("read region");
        drop(file);
        std::fs::remove_file(&path).expect("remove fixture");
        assert_eq!(
            snapshot.pixels(),
            [28, 29, 30, 31, 32, 33, 34, 35, 52, 53, 54, 55, 56, 57, 58, 59]
        );
    }
}
