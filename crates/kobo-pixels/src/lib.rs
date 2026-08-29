#![forbid(unsafe_code)]

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum PictureFormat {
    #[default]
    Gray8,
    Rgb8,
}

impl PictureFormat {
    #[must_use]
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Gray8 => 1,
            Self::Rgb8 => 3,
        }
    }

    #[must_use]
    pub fn byte_len(self, width: u32, height: u32) -> Option<usize> {
        usize::try_from(width)
            .ok()?
            .checked_mul(usize::try_from(height).ok()?)?
            .checked_mul(self.bytes_per_pixel())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PicturePixels {
    Gray8(Vec<u8>),
    Rgb8(Vec<u8>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PicturePixelsRef<'a> {
    Gray8(&'a [u8]),
    Rgb8(&'a [u8]),
}

impl PicturePixels {
    #[must_use]
    pub const fn format(&self) -> PictureFormat {
        match self {
            Self::Gray8(_) => PictureFormat::Gray8,
            Self::Rgb8(_) => PictureFormat::Rgb8,
        }
    }

    #[must_use]
    pub fn as_ref(&self) -> PicturePixelsRef<'_> {
        match self {
            Self::Gray8(bytes) => PicturePixelsRef::Gray8(bytes),
            Self::Rgb8(bytes) => PicturePixelsRef::Rgb8(bytes),
        }
    }

    #[must_use]
    pub fn byte_count(&self) -> usize {
        match self {
            Self::Gray8(bytes) | Self::Rgb8(bytes) => bytes.len(),
        }
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Gray8(bytes) | Self::Rgb8(bytes) => bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_formats_compute_checked_byte_lengths() {
        assert_eq!(PictureFormat::Gray8.byte_len(3, 2), Some(6));
        assert_eq!(PictureFormat::Rgb8.byte_len(3, 2), Some(18));
        assert_eq!(PictureFormat::Rgb8.byte_len(u32::MAX, u32::MAX), None);
    }

    #[test]
    fn picture_pixels_preserve_their_format_and_bytes() {
        let gray = PicturePixels::Gray8(vec![1, 2]);
        assert_eq!(gray.format(), PictureFormat::Gray8);
        assert_eq!(gray.as_ref(), PicturePixelsRef::Gray8(&[1, 2]));
        assert_eq!(gray.byte_count(), 2);
        assert_eq!(gray.into_bytes(), vec![1, 2]);

        let rgb = PicturePixels::Rgb8(vec![3, 4, 5]);
        assert_eq!(rgb.format(), PictureFormat::Rgb8);
        assert_eq!(rgb.as_ref(), PicturePixelsRef::Rgb8(&[3, 4, 5]));
        assert_eq!(rgb.byte_count(), 3);
        assert_eq!(rgb.into_bytes(), vec![3, 4, 5]);
    }
}
