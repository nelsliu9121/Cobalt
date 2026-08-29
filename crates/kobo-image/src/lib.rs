//! Turning pictures from the network into something an E Ink panel can show.
//!
//! Three steps, deliberately separate, because each one can fail or be refused
//! on its own: decode what arrived, fit it to the space the layout gave it, and
//! reduce it to the greys the panel can actually hold.
//!
//! Decoding is the one place in this project where bytes from a stranger are
//! parsed by code nobody here wrote, so the caps below are not decoration. A
//! JPEG header is a few bytes and can claim any size it likes; without a
//! ceiling on the decoded pixel count a hostile (or merely enormous) image
//! takes the reader's memory and, on a device with no swap and no fan, the
//! reader with it.

use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageReader};
pub use kobo_pixels::{PictureFormat, PicturePixels, PicturePixelsRef};
use std::fmt;
use std::io::Cursor;

/// The most compressed bytes a picture may arrive as.
///
/// Comfortably above a book cover or a photograph at panel size, and far below
/// anything worth streaming to a device like this.
pub const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;

/// The most pixels a picture may decode to, whatever its header claims.
///
/// BOMTOON episodes have been observed at up to 6,553,600 pixels. The
/// 7,000,000-pixel boundary admits those tall sources with measured headroom
/// while still refusing hostile dimensions before a buffer is allocated.
pub const MAX_PIXELS: u64 = 7_000_000;

/// How far [`Picture::fit_enlarging`] will blow a small picture up.
///
/// Three is where a 190 pixel book cover stops being lettering on a 300 pixel
/// per inch panel. Past it the halftone that follows has nothing left to
/// resolve and the result reads as a decoding fault.
pub const MAX_ENLARGEMENT: u32 = 3;

/// How many greys a panel can hold, unless a caller knows better.
///
/// Sixteen is what this family of controllers drives, and it is the number
/// that makes dithering worth doing: at 256 the reduction is invisible, and at
/// two everything becomes a woodcut.
pub const PANEL_GREYS: u8 = 16;

/// The most horizontal samples the fixed-point scaler holds at once.
pub const AXIS_SAMPLE_CHUNK: usize = 2_048;

/// Wraps typed picture pixels up as a PNG.
///
/// # Errors
///
/// Returns [`ImageError::Undecodable`] when the pixel byte length does not
/// match its format and dimensions, and [`ImageError::TooManyPixels`] past
/// [`MAX_PIXELS`].
pub fn encode_png(
    width: u32,
    height: u32,
    pixels: PicturePixelsRef<'_>,
) -> Result<Vec<u8>, ImageError> {
    checked_pixels(width, height)?;
    let (bytes, format, color_type) = match pixels {
        PicturePixelsRef::Gray8(bytes) => (
            bytes,
            PictureFormat::Gray8,
            image::ExtendedColorType::L8,
        ),
        PicturePixelsRef::Rgb8(bytes) => (
            bytes,
            PictureFormat::Rgb8,
            image::ExtendedColorType::Rgb8,
        ),
    };
    let expected = format.byte_len(width, height).ok_or_else(|| {
        ImageError::Undecodable("the picture byte length does not fit this platform".to_owned())
    })?;
    if bytes.len() != expected {
        return Err(ImageError::Undecodable(format!(
            "{} bytes for a {width} by {height} {format:?} picture, which needs {expected}",
            bytes.len()
        )));
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(bytes, width, height, color_type)
        .map_err(|error| ImageError::Undecodable(error.to_string()))?;
    Ok(png)
}

/// Wraps raw eight-bit grayscale framebuffer pixels up as a PNG.
///
/// # Errors
///
/// Returns the same errors as [`encode_png`].
pub fn encode_png_grey(width: u32, height: u32, grey: &[u8]) -> Result<Vec<u8>, ImageError> {
    encode_png(width, height, PicturePixelsRef::Gray8(grey))
}

/// How a picture should occupy the rectangle a component assigned to it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum FitMode {
    /// Show the whole picture and leave any remainder as paper.
    #[default]
    Contain,
    /// Contain, but enlarge small sources by at most [`MAX_ENLARGEMENT`].
    ContainEnlarging,
    /// Fill the rectangle exactly, cropping equal amounts from opposite edges.
    Cover,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImageError {
    /// More compressed bytes than [`MAX_SOURCE_BYTES`].
    TooManyBytes { bytes: usize },
    /// More pixels than [`MAX_PIXELS`].
    TooManyPixels { pixels: u64 },
    /// Nothing here recognises this as a picture.
    UnknownFormat,
    /// The decoder rejected the bytes, in its own words: "this is not a
    /// picture" and "this picture is truncated" are different things to the
    /// person looking at the screen.
    Undecodable(String),
    /// A fit was asked for into a box with no area.
    EmptyBox,
}

impl fmt::Display for ImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyBytes { bytes } => write!(
                formatter,
                "the picture is {bytes} bytes, and {MAX_SOURCE_BYTES} is the most that is read"
            ),
            Self::TooManyPixels { pixels } => write!(
                formatter,
                "the picture claims {pixels} pixels, and {MAX_PIXELS} is the most that is decoded"
            ),
            Self::UnknownFormat => {
                write!(formatter, "this is not a picture in any format read here")
            }
            Self::Undecodable(why) => write!(formatter, "the picture could not be read: {why}"),
            Self::EmptyBox => write!(formatter, "a picture cannot be fitted into no space at all"),
        }
    }
}

impl std::error::Error for ImageError {}

/// One typed picture, row-major from the top with no row padding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Picture {
    width: u32,
    height: u32,
    pixels: PicturePixels,
}

impl Picture {
    /// Builds a picture from typed bytes that are already the right shape.
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::TooManyPixels`] when the dimensions exceed
    /// [`MAX_PIXELS`], and [`ImageError::Undecodable`] when the byte length
    /// does not match the dimensions and format.
    pub fn from_pixels(
        width: u32,
        height: u32,
        pixels: PicturePixels,
    ) -> Result<Self, ImageError> {
        checked_pixels(width, height)?;
        let format = pixels.format();
        let expected = format.byte_len(width, height).ok_or_else(|| {
            ImageError::Undecodable("the picture byte length does not fit this platform".to_owned())
        })?;
        if pixels.byte_count() != expected {
            return Err(ImageError::Undecodable(format!(
                "{} bytes for a {width} by {height} {format:?} picture, which needs {expected}",
                pixels.byte_count()
            )));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Builds a Gray8 picture from bytes that are already the right shape.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::from_pixels`].
    pub fn from_grey(width: u32, height: u32, grey: Vec<u8>) -> Result<Self, ImageError> {
        Self::from_pixels(width, height, PicturePixels::Gray8(grey))
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub const fn format(&self) -> PictureFormat {
        self.pixels.format()
    }

    #[must_use]
    pub fn pixels(&self) -> PicturePixelsRef<'_> {
        self.pixels.as_ref()
    }

    #[must_use]
    pub fn into_pixels(self) -> PicturePixels {
        self.pixels
    }

    /// Scales to exactly `target_width`, keeping the aspect ratio.
    ///
    /// The operation consumes the source so an equal-width picture returns its
    /// original allocation. Other widths allocate only the final pixels and a
    /// fixed chunk of horizontal fixed-point samples.
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::EmptyBox`] for a zero source dimension or target
    /// width, and [`ImageError::TooManyPixels`] for an oversized result.
    pub fn scale_to_width(self, target_width: u32) -> Result<Self, ImageError> {
        let (width, height) = width_scaled_size((self.width, self.height), target_width)?;
        self.resample(width, height)
    }

    /// The largest size that fits inside `width` by `height` without changing
    /// the shape of the picture.
    #[must_use]
    pub fn size_within(&self, width: u32, height: u32) -> (u32, u32) {
        if self.width == 0 || self.height == 0 || width == 0 || height == 0 {
            return (0, 0);
        }
        let by_width = u64::from(width) * u64::from(self.height);
        let by_height = u64::from(height) * u64::from(self.width);
        if by_width <= by_height {
            let scaled = by_width / u64::from(self.width);
            (width, u32::try_from(scaled).unwrap_or(height).max(1))
        } else {
            let scaled = by_height / u64::from(self.height);
            (u32::try_from(scaled).unwrap_or(width).max(1), height)
        }
    }

    /// Scales the picture inside a box without enlarging a smaller source.
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::EmptyBox`] when the box has no area.
    pub fn fit(&self, width: u32, height: u32) -> Result<Self, ImageError> {
        if width == 0 || height == 0 {
            return Err(ImageError::EmptyBox);
        }
        if self.width <= width && self.height <= height {
            return Ok(self.clone());
        }
        self.scaled_to(width, height)
    }

    /// Fits inside a box while enlarging by at most [`MAX_ENLARGEMENT`].
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::EmptyBox`] when the box has no area.
    pub fn fit_enlarging(&self, width: u32, height: u32) -> Result<Self, ImageError> {
        if width == 0 || height == 0 {
            return Err(ImageError::EmptyBox);
        }
        let ceiling_width = self.width.saturating_mul(MAX_ENLARGEMENT);
        let ceiling_height = self.height.saturating_mul(MAX_ENLARGEMENT);
        self.scaled_to(width.min(ceiling_width), height.min(ceiling_height))
    }

    /// Prepares a picture with an explicit fit policy.
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::EmptyBox`] for a zero-sized target and
    /// [`ImageError::TooManyPixels`] before an oversized result is allocated.
    pub fn prepare(&self, width: u32, height: u32, mode: FitMode) -> Result<Self, ImageError> {
        match mode {
            FitMode::Contain => self.fit(width, height),
            FitMode::ContainEnlarging => self.fit_enlarging(width, height),
            FitMode::Cover => self.cover(width, height),
        }
    }

    /// Fills a box, preserving aspect ratio and cropping from the centre.
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::EmptyBox`] for a zero-sized target and
    /// [`ImageError::TooManyPixels`] before an oversized result is allocated.
    pub fn cover(&self, width: u32, height: u32) -> Result<Self, ImageError> {
        checked_pixels(width, height)?;
        if width == 0 || height == 0 || self.width == 0 || self.height == 0 {
            return Err(ImageError::EmptyBox);
        }
        let width_limited =
            u64::from(width) * u64::from(self.height) >= u64::from(height) * u64::from(self.width);
        let (scaled_width, scaled_height) = if width_limited {
            (
                width,
                div_ceil(u64::from(width) * u64::from(self.height), self.width),
            )
        } else {
            (
                div_ceil(u64::from(height) * u64::from(self.width), self.height),
                height,
            )
        };
        let left = (scaled_width - width) / 2;
        let top = (scaled_height - height) / 2;
        self.clone().resample_region(
            width,
            height,
            scaled_width,
            scaled_height,
            left,
            top,
        )
    }

    fn scaled_to(&self, width: u32, height: u32) -> Result<Self, ImageError> {
        let (target_width, target_height) = self.size_within(width, height);
        if target_width == 0 || target_height == 0 {
            return Err(ImageError::EmptyBox);
        }
        self.clone().resample(target_width, target_height)
    }

    fn resample(self, width: u32, height: u32) -> Result<Self, ImageError> {
        if width == self.width && height == self.height {
            return Ok(self);
        }
        self.resample_region(width, height, width, height, 0, 0)
    }

    fn resample_region(
        self,
        width: u32,
        height: u32,
        mapped_width: u32,
        mapped_height: u32,
        left: u32,
        top: u32,
    ) -> Result<Self, ImageError> {
        checked_pixels(width, height)?;
        if width == 0
            || height == 0
            || self.width == 0
            || self.height == 0
            || mapped_width == 0
            || mapped_height == 0
        {
            return Err(ImageError::EmptyBox);
        }
        let format = self.format();
        let source_width = self.width;
        let source_height = self.height;
        let source = self.pixels.into_bytes();
        let bytes = match format {
            PictureFormat::Gray8 => resample_bytes::<1>(
                &source,
                source_width,
                source_height,
                width,
                height,
                mapped_width,
                mapped_height,
                left,
                top,
            ),
            PictureFormat::Rgb8 => resample_bytes::<3>(
                &source,
                source_width,
                source_height,
                width,
                height,
                mapped_width,
                mapped_height,
                left,
                top,
            ),
        };
        let pixels = match format {
            PictureFormat::Gray8 => PicturePixels::Gray8(bytes),
            PictureFormat::Rgb8 => PicturePixels::Rgb8(bytes),
        };
        Self::from_pixels(width, height, pixels)
    }

    /// Reduces Gray8 pixels to evenly spaced levels with Floyd–Steinberg error
    /// diffusion.
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::Undecodable`] for an RGB8 picture.
    pub fn dither(&mut self, levels: u8) -> Result<(), ImageError> {
        let PicturePixels::Gray8(grey) = &mut self.pixels else {
            return Err(ImageError::Undecodable(
                "dithering requires a grayscale picture".to_owned(),
            ));
        };
        let levels = u32::from(levels.max(2));
        let width = self.width as usize;
        let height = self.height as usize;
        if width == 0 || height == 0 {
            return Ok(());
        }
        let mut current_error = vec![0_i32; width];
        let mut next_error = vec![0_i32; width];
        for y in 0..height {
            for x in 0..width {
                let index = y * width + x;
                let wanted = i32::from(grey[index]) + current_error[x];
                let quantized = nearest_level(wanted, levels);
                grey[index] = u8::try_from(quantized.clamp(0, 255)).unwrap_or(255);
                let residue = wanted - quantized;
                if residue == 0 {
                    continue;
                }
                if x + 1 < width {
                    current_error[x + 1] += residue * 7 / 16;
                }
                if y + 1 < height {
                    if x > 0 {
                        next_error[x - 1] += residue * 3 / 16;
                    }
                    next_error[x] += residue * 5 / 16;
                    if x + 1 < width {
                        next_error[x + 1] += residue / 16;
                    }
                }
            }
            std::mem::swap(&mut current_error, &mut next_error);
            next_error.fill(0);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AxisSample {
    low: u32,
    high: u32,
    upper_weight: u32,
}

fn axis_sample(position: u32, source_extent: u32, mapped_extent: u32) -> AxisSample {
    if source_extent <= 1 || mapped_extent <= 1 {
        return AxisSample::default();
    }
    let denominator = u64::from(mapped_extent - 1);
    let numerator = u64::from(position) * u64::from(source_extent - 1);
    let low = u32::try_from(numerator / denominator).unwrap_or(source_extent - 1);
    AxisSample {
        low,
        high: low.saturating_add(1).min(source_extent - 1),
        upper_weight: u32::try_from(numerator % denominator).unwrap_or(0),
    }
}

fn axis_denominator(mapped_extent: u32) -> u64 {
    u64::from(mapped_extent.saturating_sub(1).max(1))
}

#[allow(clippy::too_many_arguments)]
fn resample_bytes<const CHANNELS: usize>(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    width: u32,
    height: u32,
    mapped_width: u32,
    mapped_height: u32,
    left: u32,
    top: u32,
) -> Vec<u8> {
    let output_len = usize::try_from(u64::from(width) * u64::from(height))
        .expect("bounded picture pixels fit usize")
        .checked_mul(CHANNELS)
        .expect("bounded picture bytes fit usize");
    let mut output = Vec::with_capacity(output_len);
    let mut horizontal = Vec::with_capacity(AXIS_SAMPLE_CHUNK);
    let horizontal_denominator = axis_denominator(mapped_width);
    let vertical_denominator = axis_denominator(mapped_height);
    let denominator = horizontal_denominator * vertical_denominator;

    for y in 0..height {
        let vertical = axis_sample(top + y, source_height, mapped_height);
        let upper_y = u64::from(vertical.upper_weight);
        let lower_y = vertical_denominator - upper_y;
        for chunk_start in (0..width).step_by(AXIS_SAMPLE_CHUNK) {
            horizontal.clear();
            let chunk_len = (width - chunk_start).min(AXIS_SAMPLE_CHUNK as u32);
            for offset in 0..chunk_len {
                horizontal.push(axis_sample(
                    left + chunk_start + offset,
                    source_width,
                    mapped_width,
                ));
            }
            for sample in &horizontal {
                let upper_x = u64::from(sample.upper_weight);
                let lower_x = horizontal_denominator - upper_x;
                for channel in 0..CHANNELS {
                    let upper_left = u64::from(source[source_index::<CHANNELS>(
                        source_width,
                        sample.low,
                        vertical.low,
                        channel,
                    )]);
                    let upper_right = u64::from(source[source_index::<CHANNELS>(
                        source_width,
                        sample.high,
                        vertical.low,
                        channel,
                    )]);
                    let lower_left = u64::from(source[source_index::<CHANNELS>(
                        source_width,
                        sample.low,
                        vertical.high,
                        channel,
                    )]);
                    let lower_right = u64::from(source[source_index::<CHANNELS>(
                        source_width,
                        sample.high,
                        vertical.high,
                        channel,
                    )]);
                    let upper = upper_left * lower_x + upper_right * upper_x;
                    let lower = lower_left * lower_x + lower_right * upper_x;
                    let value = (upper * lower_y + lower * upper_y + denominator / 2)
                        / denominator;
                    output.push(u8::try_from(value).expect("interpolation stays in one byte"));
                }
            }
        }
    }
    debug_assert_eq!(output.len(), output_len);
    output
}

fn source_index<const CHANNELS: usize>(
    width: u32,
    x: u32,
    y: u32,
    channel: usize,
) -> usize {
    let pixel = u64::from(y) * u64::from(width) + u64::from(x);
    usize::try_from(pixel)
        .expect("bounded source index fits usize")
        .checked_mul(CHANNELS)
        .and_then(|index| index.checked_add(channel))
        .expect("bounded source byte index fits usize")
}

fn div_ceil(numerator: u64, denominator: u32) -> u32 {
    let denominator = u64::from(denominator);
    u32::try_from(numerator.div_ceil(denominator)).unwrap_or(u32::MAX)
}

fn checked_pixels(width: u32, height: u32) -> Result<u64, ImageError> {
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PIXELS {
        return Err(ImageError::TooManyPixels { pixels });
    }
    Ok(pixels)
}

fn nearest_level(value: i32, levels: u32) -> i32 {
    let steps = i32::try_from(levels - 1).unwrap_or(1).max(1);
    let clamped = value.clamp(0, 255);
    let step = (clamped * steps + 127) / 255;
    (step * 255 + steps / 2) / steps
}

/// How large a picture will be, without decoding it.
///
/// Reads the header and stops. Every format here states its own dimensions in
/// the first few dozen bytes, so this costs microseconds where [`decode`] costs
/// a hundred milliseconds for a plate off a Kobo's processor -- and a layout
/// that only needs to know how much room to leave should not be paying for
/// pixels it is not going to draw yet. The reader uses it to measure an
/// illustrated book at the moment it opens and decode the plates afterwards,
/// which is the difference between a book opening at once and the panel
/// freezing for three seconds.
///
/// Orientation is applied, so a photograph the camera wrote sideways is
/// reported the way it will be drawn rather than the way it was stored.
///
/// # Errors
///
/// The same refusals as [`decode`], for the same reasons: a header claiming
/// more than [`MAX_PIXELS`] is a header worth refusing before anyone acts on
/// it.
pub fn size(bytes: &[u8]) -> Result<(u32, u32), ImageError> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(ImageError::TooManyBytes { bytes: bytes.len() });
    }
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| ImageError::Undecodable(error.to_string()))?;
    if reader.format().is_none() {
        return Err(ImageError::UnknownFormat);
    }
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| ImageError::Undecodable(error.to_string()))?;
    let orientation = decoder
        .orientation()
        .map_err(|error| ImageError::Undecodable(error.to_string()))?;
    let (width, height) = decoder.dimensions();
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PIXELS {
        return Err(ImageError::TooManyPixels { pixels });
    }
    // A quarter turn swaps the two. Everything else -- the flips, and no
    // orientation at all -- leaves them as they were stored.
    Ok(match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (height, width),
        _ => (width, height),
    })
}

/// What [`Picture::fit`] will make of a picture `source` pixels across.
///
/// Answered from dimensions alone, so a caller holding nothing but the reply
/// from [`size`] can work out how much room the picture will take before
/// deciding to decode it. That is what lets an illustrated book be measured
/// the moment it opens and drawn a plate at a time afterwards, with no page
/// changing size when the pixels arrive.
///
/// Not the same answer as [`Picture::size_within`], which scales a small
/// picture up to its box. `fit` deliberately leaves one alone, so this does
/// too: this predicts `fit`, and a prediction that disagreed with the thing it
/// predicts would be worse than none.
#[must_use]
pub fn fitted_size(source: (u32, u32), width: u32, height: u32) -> (u32, u32) {
    let (source_width, source_height) = source;
    if source_width == 0 || source_height == 0 || width == 0 || height == 0 {
        return (0, 0);
    }
    // A picture already inside the box is left alone, the way `fit` leaves it.
    if source_width <= width && source_height <= height {
        return source;
    }
    let by_width = u64::from(width) * u64::from(source_height);
    let by_height = u64::from(height) * u64::from(source_width);
    if by_width <= by_height {
        let scaled = by_width / u64::from(source_width);
        (width, u32::try_from(scaled).unwrap_or(height).max(1))
    } else {
        let scaled = by_height / u64::from(source_height);
        (u32::try_from(scaled).unwrap_or(width).max(1), height)
    }
}

/// The exact-width size of a source image, preserving its aspect ratio.
///
/// The height uses the same integer rounding as [`Picture::scale_to_width`],
/// so manifest planning and decoded images cannot disagree.
///
/// # Errors
///
/// Returns [`ImageError::EmptyBox`] for a zero source dimension or target
/// width, and [`ImageError::TooManyPixels`] when the result exceeds
/// [`MAX_PIXELS`].
pub fn width_scaled_size(source: (u32, u32), target_width: u32) -> Result<(u32, u32), ImageError> {
    if source.0 == 0 || source.1 == 0 || target_width == 0 {
        return Err(ImageError::EmptyBox);
    }
    let target_height =
        u32::try_from((u64::from(target_width) * u64::from(source.1)) / u64::from(source.0))
            .unwrap_or(u32::MAX)
            .max(1);
    checked_pixels(target_width, target_height)?;
    Ok((target_width, target_height))
}

/// Reads a picture in the compatibility Gray8 format.
///
/// # Errors
///
/// Refuses anything over [`MAX_SOURCE_BYTES`] before looking at it, anything
/// claiming more than [`MAX_PIXELS`] before allocating for it, and reports
/// whatever the decoder said otherwise.
pub fn decode(bytes: &[u8]) -> Result<Picture, ImageError> {
    decode_picture(bytes, PictureFormat::Gray8, None)
}

/// Reads a WebP into the explicitly requested pixel format.
///
/// # Errors
///
/// Applies the same byte and pixel limits as [`decode`] and refuses every
/// guessed source format other than WebP before decoding pixel storage.
pub fn decode_webp(bytes: &[u8], format: PictureFormat) -> Result<Picture, ImageError> {
    decode_picture(bytes, format, Some(image::ImageFormat::WebP))
}

fn decode_picture(
    bytes: &[u8],
    format: PictureFormat,
    required: Option<image::ImageFormat>,
) -> Result<Picture, ImageError> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(ImageError::TooManyBytes { bytes: bytes.len() });
    }
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| ImageError::Undecodable(error.to_string()))?;
    let detected = reader.format().ok_or(ImageError::UnknownFormat)?;
    if required.is_some_and(|required| required != detected) {
        return Err(ImageError::Undecodable(
            "the typed decoder accepts WebP only".to_owned(),
        ));
    }
    let (width, height) = reader
        .into_dimensions()
        .map_err(|error| ImageError::Undecodable(error.to_string()))?;
    checked_pixels(width, height)?;

    let mut decoder = ImageReader::with_format(Cursor::new(bytes), detected)
        .into_decoder()
        .map_err(|error| ImageError::Undecodable(error.to_string()))?;
    let orientation = decoder
        .orientation()
        .map_err(|error| ImageError::Undecodable(error.to_string()))?;
    let mut image = DynamicImage::from_decoder(decoder)
        .map_err(|error| ImageError::Undecodable(error.to_string()))?;
    image.apply_orientation(orientation);

    match format {
        PictureFormat::Gray8 => {
            let luma = image.to_luma_alpha8();
            let mut grey = Vec::with_capacity(
                PictureFormat::Gray8
                    .byte_len(luma.width(), luma.height())
                    .ok_or_else(|| {
                        ImageError::Undecodable(
                            "the decoded picture byte length does not fit this platform".to_owned(),
                        )
                    })?,
            );
            for pixel in luma.pixels() {
                grey.push(over_white(pixel[0], pixel[1]));
            }
            Picture::from_pixels(
                luma.width(),
                luma.height(),
                PicturePixels::Gray8(grey),
            )
        }
        PictureFormat::Rgb8 => {
            let rgba = image.to_rgba8();
            let mut rgb = Vec::with_capacity(
                PictureFormat::Rgb8
                    .byte_len(rgba.width(), rgba.height())
                    .ok_or_else(|| {
                        ImageError::Undecodable(
                            "the decoded picture byte length does not fit this platform".to_owned(),
                        )
                    })?,
            );
            for pixel in rgba.pixels() {
                rgb.push(over_white(pixel[0], pixel[3]));
                rgb.push(over_white(pixel[1], pixel[3]));
                rgb.push(over_white(pixel[2], pixel[3]));
            }
            Picture::from_pixels(rgba.width(), rgba.height(), PicturePixels::Rgb8(rgb))
        }
    }
}

fn over_white(channel: u8, alpha: u8) -> u8 {
    let a = u16::from(alpha);
    u8::try_from((u16::from(channel) * a + 255 * (255 - a) + 127) / 255)
        .expect("white composite is one byte")
}

#[cfg(test)]
mod tests {
    use super::{
        decode, decode_webp, encode_png, fitted_size, size, width_scaled_size, AxisSample, FitMode,
        ImageError, Picture, AXIS_SAMPLE_CHUNK, MAX_ENLARGEMENT, MAX_PIXELS, PANEL_GREYS,
    };
    use image::{DynamicImage, ImageFormat, RgbImage, RgbaImage};
    use kobo_pixels::{PictureFormat, PicturePixels, PicturePixelsRef};
    use std::io::Cursor;

    fn gray(picture: &Picture) -> &[u8] {
        let PicturePixelsRef::Gray8(gray) = picture.pixels() else {
            panic!("test fixture unexpectedly became RGB");
        };
        gray
    }

    /// A real four by four JPEG, so the decoder is exercised against a file
    /// rather than against a mock of one.
    fn tiny_jpeg() -> Vec<u8> {
        const BASE64: &str = concat!(
            "/9j/4AAQSkZJRgABAQEAYABgAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0a",
            "HBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/wAALCAAEAAQBAREA/8QAHwAAAQUBAQEB",
            "AQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1Fh",
            "ByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZ",
            "WmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXG",
            "x8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/9oACAEBAAA/APn+v//Z"
        );
        decode_base64(BASE64)
    }

    fn tiny_lossy_webp() -> Vec<u8> {
        decode_base64("UklGRiQAAABXRUJQVlA4IBgAAAAwAQCdASoBAAEAAUAmJaQAA3AA/vuUAAA=")
    }

    fn transparent_lossless_webp() -> Vec<u8> {
        decode_base64(
            "UklGRkIAAABXRUJQVlA4TDYAAAAvAQAAEM1VICICEeGBBAAAAAAAnL8AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAYAo=",
        )
    }

    fn decode_base64(text: &str) -> Vec<u8> {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        let mut accumulator = 0_u32;
        let mut bits = 0_u32;
        for byte in text.bytes() {
            if byte == b'=' {
                break;
            }
            let Some(value) = ALPHABET.iter().position(|candidate| *candidate == byte) else {
                continue;
            };
            accumulator = (accumulator << 6) | u32::try_from(value).unwrap_or(0);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push(u8::try_from((accumulator >> bits) & 0xff).unwrap_or(0));
            }
        }
        out
    }

    fn png(width: u32, height: u32, rgba: Vec<u8>) -> Vec<u8> {
        let image = RgbaImage::from_raw(width, height, rgba).expect("rgba fixture");
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, ImageFormat::Png)
            .expect("encode png");
        bytes.into_inner()
    }

    fn rgba_webp(width: u32, height: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
        let rgba = pixels
            .iter()
            .flat_map(|pixel| pixel.iter().copied())
            .collect::<Vec<_>>();
        let mut bytes = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
            .encode(
                &rgba,
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .expect("encode WebP fixture");
        bytes
    }

    fn oriented_jpeg() -> Vec<u8> {
        let image = RgbImage::from_raw(2, 1, vec![255, 0, 0, 0, 0, 0]).expect("rgb fixture");
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image)
            .write_to(&mut encoded, ImageFormat::Jpeg)
            .expect("encode jpeg");
        let jpeg = encoded.into_inner();

        // Minimal little-endian Exif IFD carrying orientation 6 (rotate 90°).
        let exif: [u8; 36] = [
            0xff, 0xe1, 0x00, 0x22, b'E', b'x', b'i', b'f', 0, 0, b'I', b'I', 0x2a, 0, 8, 0, 0, 0,
            1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
        ];
        let mut result = Vec::with_capacity(jpeg.len() + exif.len());
        result.extend_from_slice(&jpeg[..2]);
        result.extend_from_slice(&exif);
        result.extend_from_slice(&jpeg[2..]);
        result
    }

    #[test]
    fn rgb_decode_preserves_color_and_composites_alpha_on_white() {
        let webp = rgba_webp(2, 1, &[[255, 0, 0, 255], [0, 0, 255, 128]]);
        let picture = decode_webp(&webp, PictureFormat::Rgb8).expect("RGB WebP");
        assert_eq!(
            picture.pixels(),
            PicturePixelsRef::Rgb8(&[255, 0, 0, 127, 127, 255])
        );
    }

    #[test]
    fn generic_decode_remains_perceptual_gray_with_alpha_on_white() {
        let webp = rgba_webp(2, 1, &[[255, 0, 0, 255], [0, 0, 255, 128]]);
        let picture = decode(&webp).expect("generic WebP");
        assert_eq!(picture.format(), PictureFormat::Gray8);
        assert_eq!(picture.pixels(), PicturePixelsRef::Gray8(&[54, 136]));
    }

    #[test]
    fn rgb_scaling_uses_exact_endpoint_aligned_bilinear_output() {
        let picture = Picture::from_pixels(
            2,
            2,
            PicturePixels::Rgb8(vec![
                255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255,
            ]),
        )
        .expect("RGB source");
        let scaled = picture.scale_to_width(3).expect("scale");
        assert_eq!((scaled.width(), scaled.height()), (3, 3));
        assert_eq!(
            scaled.pixels(),
            PicturePixelsRef::Rgb8(&[
                255, 0, 0, 128, 128, 0, 0, 255, 0, 128, 0, 128, 128, 128, 128, 128, 255, 128,
                0, 0, 255, 128, 128, 255, 255, 255, 255,
            ])
        );
    }

    #[test]
    fn constant_color_survives_enlarging_and_reducing_in_both_formats() {
        for pixels in [
            PicturePixels::Gray8(vec![73; 16]),
            PicturePixels::Rgb8([11, 97, 203].repeat(16)),
        ] {
            let format = pixels.format();
            let reduced = Picture::from_pixels(4, 4, pixels)
                .expect("constant source")
                .scale_to_width(2)
                .expect("reduce");
            let enlarged = reduced.scale_to_width(8).expect("enlarge");
            let expected = match format {
                PictureFormat::Gray8 => PicturePixelsRef::Gray8(&[73; 64]),
                PictureFormat::Rgb8 => {
                    let expected = [11, 97, 203].repeat(64);
                    assert_eq!(enlarged.pixels(), PicturePixelsRef::Rgb8(&expected));
                    continue;
                }
            };
            assert_eq!(enlarged.pixels(), expected);
        }
    }

    #[test]
    fn same_width_scaling_reuses_the_owned_pixel_allocation() {
        let picture = Picture::from_pixels(
            2,
            1,
            PicturePixels::Rgb8(vec![1, 2, 3, 4, 5, 6]),
        )
        .expect("RGB source");
        let before = match picture.pixels() {
            PicturePixelsRef::Rgb8(bytes) => bytes.as_ptr(),
            PicturePixelsRef::Gray8(_) => panic!("RGB source changed format"),
        };
        let unchanged = picture.scale_to_width(2).expect("same width");
        let after = match unchanged.pixels() {
            PicturePixelsRef::Rgb8(bytes) => bytes.as_ptr(),
            PicturePixelsRef::Gray8(_) => panic!("RGB result changed format"),
        };
        assert_eq!(before, after);
    }

    #[test]
    fn typed_webp_decode_refuses_png_even_when_it_is_decodable() {
        let png = png(1, 1, vec![1, 2, 3, 255]);
        assert!(decode_webp(&png, PictureFormat::Gray8).is_err());
        assert!(decode_webp(&png, PictureFormat::Rgb8).is_err());
    }

    #[test]
    fn scaler_handles_every_supported_panel_width_from_1080() {
        let picture = Picture::from_pixels(
            1080,
            1,
            PicturePixels::Rgb8(
                (0..1080)
                    .flat_map(|x| {
                        let value = u8::try_from(x % 256).unwrap_or(0);
                        [value, 255 - value, value / 2]
                    })
                    .collect(),
            ),
        )
        .expect("RGB source");
        for width in [1072, 1264, 1404] {
            let scaled = picture.clone().scale_to_width(width).expect("scale");
            assert_eq!((scaled.width(), scaled.height()), (width, 1));
            let PicturePixelsRef::Rgb8(bytes) = scaled.pixels() else {
                panic!("RGB scale changed format");
            };
            assert_eq!(
                bytes.len(),
                usize::try_from(width).expect("panel width") * 3
            );
        }
    }

    #[test]
    fn consuming_scaler_rejects_zero_width_and_oversized_output() {
        let source = Picture::from_grey(1, 1, vec![0]).expect("source");
        assert_eq!(
            source.clone().scale_to_width(0),
            Err(ImageError::EmptyBox)
        );
        let tall = Picture::from_grey(1, 7_000_000, vec![0; 7_000_000]).expect("tall source");
        assert!(matches!(
            tall.scale_to_width(2),
            Err(ImageError::TooManyPixels { .. })
        ));
    }

    #[test]
    fn maximum_width_thin_image_keeps_axis_scratch_bounded() {
        assert_eq!(
            std::mem::size_of::<AxisSample>() * AXIS_SAMPLE_CHUNK,
            24_576
        );
        let picture =
            Picture::from_grey(6_999_999, 1, vec![19; 6_999_999]).expect("thin source");
        let scaled = picture.scale_to_width(7_000_000).expect("boundary scale");
        assert_eq!((scaled.width(), scaled.height()), (7_000_000, 1));
        assert_eq!(
            scaled.pixels(),
            PicturePixelsRef::Gray8(&vec![19; 7_000_000])
        );
    }

    #[test]
    fn rgb_dithering_is_refused_without_changing_pixels() {
        let mut picture =
            Picture::from_pixels(1, 1, PicturePixels::Rgb8(vec![1, 2, 3])).expect("RGB source");
        assert!(picture.dither(PANEL_GREYS).is_err());
        assert_eq!(picture.pixels(), PicturePixelsRef::Rgb8(&[1, 2, 3]));
    }

    #[test]
    fn png_evidence_preserves_typed_pixels_and_checks_lengths() {
        let gray = encode_png(2, 1, PicturePixelsRef::Gray8(&[17, 231])).expect("gray PNG");
        assert_eq!(
            image::load_from_memory_with_format(&gray, ImageFormat::Png)
                .expect("read gray PNG")
                .to_luma8()
                .into_raw(),
            vec![17, 231]
        );

        let rgb =
            encode_png(2, 1, PicturePixelsRef::Rgb8(&[255, 0, 0, 0, 255, 7])).expect("RGB PNG");
        assert_eq!(
            image::load_from_memory_with_format(&rgb, ImageFormat::Png)
                .expect("read RGB PNG")
                .to_rgb8()
                .into_raw(),
            vec![255, 0, 0, 0, 255, 7]
        );

        assert!(encode_png(2, 1, PicturePixelsRef::Gray8(&[0])).is_err());
        assert!(encode_png(2, 1, PicturePixelsRef::Rgb8(&[0; 5])).is_err());
    }

    #[test]
    fn a_real_jpeg_decodes_to_grey_bytes() {
        let picture = decode(&tiny_jpeg()).expect("decode the jpeg");
        assert_eq!(picture.width(), 4);
        assert_eq!(picture.height(), 4);
        assert_eq!(gray(&picture).len(), 16);
    }

    #[test]
    fn a_real_lossy_webp_decodes_to_grey_bytes() {
        let picture = decode(&tiny_lossy_webp()).expect("decode the WebP");
        assert_eq!((picture.width(), picture.height()), (1, 1));
        assert_eq!(gray(&picture), &[234]);
    }

    #[test]
    fn transparent_webp_pixels_are_composited_onto_paper() {
        let picture = decode(&transparent_lossless_webp()).expect("decode the WebP");
        assert_eq!((picture.width(), picture.height()), (2, 1));
        assert_eq!(gray(&picture), &[255, 0]);
    }

    #[test]
    fn webp_header_size_matches_decode() {
        for webp in [tiny_lossy_webp(), transparent_lossless_webp()] {
            let picture = decode(&webp).expect("decode the WebP");
            assert_eq!(
                size(&webp).expect("read the WebP header"),
                (picture.width(), picture.height())
            );
        }
    }

    #[test]
    fn transparent_pixels_are_composited_onto_paper() {
        let picture = decode(&png(2, 1, vec![0, 0, 0, 0, 0, 0, 0, 255])).expect("png");
        assert_eq!(gray(&picture), &[255, 0]);
    }

    #[test]
    fn colour_is_reduced_by_perceptual_weight_rather_than_by_average() {
        // An averaged red and an averaged blue come out the same grey, which
        // on a book cover is the title disappearing into its background.
        let picture = decode(&png(
            3,
            1,
            vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255],
        ))
        .expect("png");
        let [red, green, blue] =
            <[u8; 3]>::try_from(gray(&picture)).expect("three pixels");
        assert!(green > red && red > blue, "{red} {green} {blue}");
    }

    #[test]
    fn a_half_transparent_pixel_lands_between_its_colour_and_the_paper() {
        let opaque = decode(&png(1, 1, vec![0, 0, 0, 255])).expect("png");
        let half = decode(&png(1, 1, vec![0, 0, 0, 128])).expect("png");
        assert_eq!(gray(&opaque), &[0]);
        assert_eq!(gray(&half), &[127]);
    }

    #[test]
    fn jpeg_exif_orientation_is_applied_before_layout() {
        let picture = decode(&oriented_jpeg()).expect("oriented jpeg");
        assert_eq!((picture.width(), picture.height()), (1, 2));
    }

    #[test]
    fn something_that_is_not_a_picture_is_refused_rather_than_guessed_at() {
        let error = decode(b"<!doctype html><title>not a picture</title>").expect_err("refused");
        assert!(matches!(
            error,
            ImageError::UnknownFormat | ImageError::Undecodable(_)
        ));
    }

    /// The decoder reads bytes from a stranger, so the ceiling has to hold
    /// before anything is allocated rather than after.
    #[test]
    fn an_enormous_claim_is_refused_before_it_is_believed() {
        let error = Picture::from_grey(100_000, 100_000, Vec::new()).expect_err("refused");
        assert!(matches!(error, ImageError::TooManyPixels { .. }));
        assert!(u64::from(100_000_u32) * u64::from(100_000_u32) > MAX_PIXELS);
    }

    #[test]
    fn shared_pixel_limit_accepts_exactly_seven_million() {
        assert!(Picture::from_grey(2_000, 3_500, vec![0; 7_000_000]).is_ok());
        assert_eq!(
            Picture::from_grey(1, 7_000_001, vec![0; 7_000_001]),
            Err(ImageError::TooManyPixels { pixels: 7_000_001 })
        );
    }

    #[test]
    fn exact_width_size_and_resize_share_rounding() {
        assert_eq!(width_scaled_size((4, 8), 2), Ok((2, 4)));
        let source = Picture::from_grey(4, 8, (0..32).collect()).expect("source");
        let scaled = source.scale_to_width(2).expect("scale");
        assert_eq!((scaled.width(), scaled.height()), (2, 4));
    }

    #[test]
    fn width_scaling_is_explicitly_allowed_to_enlarge() {
        let source = Picture::from_grey(1, 1, vec![127]).expect("source");
        let scaled = source.scale_to_width(3).expect("scale");
        assert_eq!((scaled.width(), scaled.height()), (3, 3));
    }

    #[test]
    fn width_scaling_rejects_target_allocation_before_resize() {
        assert!(matches!(
            width_scaled_size((1, 7_000_000), 2),
            Err(ImageError::TooManyPixels { .. })
        ));
    }

    #[test]
    fn width_scaling_rejects_empty_dimensions() {
        assert_eq!(width_scaled_size((0, 1), 1), Err(ImageError::EmptyBox));
        assert_eq!(width_scaled_size((1, 0), 1), Err(ImageError::EmptyBox));
        assert_eq!(width_scaled_size((1, 1), 0), Err(ImageError::EmptyBox));
    }

    #[test]
    fn grey_bytes_that_are_not_the_size_they_claim_are_refused() {
        let error = Picture::from_grey(4, 4, vec![0; 15]).expect_err("refused");
        assert!(matches!(error, ImageError::Undecodable(_)));
    }

    #[test]
    fn fitting_keeps_the_shape_of_the_picture() {
        // A book cover: taller than it is wide, which is the case a square box
        // gets wrong if it scales both axes independently.
        let picture = Picture::from_grey(190, 300, vec![128; 190 * 300]).expect("build");
        let fitted = picture.fit(120, 120).expect("fit");
        assert_eq!(fitted.height(), 120, "the tall edge is the one that binds");
        assert_eq!(fitted.width(), 76, "190/300 of 120, kept exactly");
        assert_eq!(gray(&fitted).len(), 76 * 120);
    }

    #[test]
    fn cover_fills_the_box_and_crops_from_the_centre() {
        let picture =
            Picture::from_grey(4, 2, vec![10, 20, 30, 40, 10, 20, 30, 40]).expect("build");
        let covered = picture.prepare(2, 2, FitMode::Cover).expect("cover");
        assert_eq!((covered.width(), covered.height()), (2, 2));
        assert_eq!(gray(&covered), &[20, 30, 20, 30]);
    }

    #[test]
    fn resize_targets_are_bounded_before_allocation() {
        let picture = Picture::from_grey(1, 1, vec![0]).expect("build");
        assert!(matches!(
            picture.cover(u32::MAX, u32::MAX),
            Err(ImageError::TooManyPixels { .. })
        ));
    }

    #[test]
    fn a_picture_smaller_than_its_box_is_left_alone() {
        let picture = Picture::from_grey(20, 30, vec![7; 600]).expect("build");
        let fitted = picture.fit(400, 400).expect("fit");
        assert_eq!((fitted.width(), fitted.height()), (20, 30));
        assert_eq!(gray(&fitted), gray(&picture), "and not resampled either");
    }

    /// The defect this exists for: a Gutenberg cover is 190 by 300, a portrait
    /// tile on the Clara is more than twice that, and `fit` left the cover at
    /// its published size floating in an empty cell.
    #[test]
    fn a_cover_given_a_tile_of_its_own_is_enlarged_to_most_of_it() {
        let picture = Picture::from_grey(190, 300, vec![128; 190 * 300]).expect("build");
        let filled = picture.fit_enlarging(480, 660).expect("fit");
        assert_eq!(filled.height(), 660, "the tall edge is the one that binds");
        assert_eq!(filled.width(), 418, "190/300 of 660, kept exactly");
        assert_eq!(gray(&filled).len(), 418 * 660);
    }

    #[test]
    fn enlargement_stops_before_a_picture_becomes_a_blur() {
        let picture = Picture::from_grey(40, 40, vec![9; 1600]).expect("build");
        let filled = picture.fit_enlarging(4000, 4000).expect("fit");
        assert_eq!(
            (filled.width(), filled.height()),
            (40 * MAX_ENLARGEMENT, 40 * MAX_ENLARGEMENT),
            "the cap decides, not the box"
        );
    }

    #[test]
    fn enlarging_still_never_overflows_a_box_a_picture_already_exceeds() {
        let picture = Picture::from_grey(1000, 1000, vec![9; 1_000_000]).expect("build");
        let filled = picture.fit_enlarging(100, 100).expect("fit");
        assert_eq!((filled.width(), filled.height()), (100, 100));
    }

    /// The whole point of reading a header: the answer has to be the same one
    /// decoding would have given, or a page measured from it moves when the
    /// pixels turn up -- which is the thing this was written to stop.
    #[test]
    fn the_size_read_from_a_header_is_the_size_the_decoder_returns() {
        let jpeg = tiny_jpeg();
        let decoded = decode(&jpeg).expect("decode");
        assert_eq!(
            size(&jpeg).expect("header"),
            (decoded.width(), decoded.height())
        );

        let png = png(37, 91, vec![200; 37 * 91 * 4]);
        let decoded = decode(&png).expect("decode");
        assert_eq!(
            size(&png).expect("header"),
            (decoded.width(), decoded.height())
        );
    }

    /// And the same for the size it will be drawn at, which is what a layout
    /// actually reserves room for.
    #[test]
    fn a_predicted_fit_is_the_fit_that_happens() {
        for (width, height) in [(37, 91), (400, 200), (16, 16)] {
            let picture = Picture::from_grey(width, height, vec![64; (width * height) as usize])
                .expect("build");
            for (box_width, box_height) in [(100, 100), (945, 1063), (10, 4000)] {
                let fitted = picture.fit(box_width, box_height).expect("fit");
                assert_eq!(
                    fitted_size((width, height), box_width, box_height),
                    (fitted.width(), fitted.height()),
                    "a {width} by {height} picture in a {box_width} by {box_height} box"
                );
            }
        }
    }

    #[test]
    fn a_box_with_no_area_is_refused_rather_than_producing_nothing() {
        let picture = Picture::from_grey(4, 4, vec![0; 16]).expect("build");
        assert_eq!(
            picture.fit(0, 10).expect_err("refused"),
            ImageError::EmptyBox
        );
    }

    #[test]
    fn dithering_leaves_only_the_greys_the_panel_can_hold() {
        // A gradient, which is exactly what bands when it is simply rounded.
        let grey = (0..=255_u8).collect::<Vec<_>>();
        let mut picture = Picture::from_grey(16, 16, grey).expect("build");
        picture.dither(PANEL_GREYS).expect("dither Gray8");
        let allowed = (0..u32::from(PANEL_GREYS))
            .map(|step| u8::try_from((step * 255 + 7) / 15).unwrap_or(255))
            .collect::<Vec<_>>();
        for value in gray(&picture) {
            assert!(
                allowed.contains(value),
                "{value} is not one of the {PANEL_GREYS} greys: {allowed:?}"
            );
        }
    }

    /// Error diffusion that rounds inside the pixel buffer drifts, and a
    /// photograph that comes out visibly darker than it went in is the usual
    /// symptom. The mean has to survive the reduction.
    #[test]
    fn dithering_keeps_the_overall_brightness() {
        let grey = (0..1024)
            .map(|index| u8::try_from(index % 256).unwrap_or(0))
            .collect::<Vec<_>>();
        let picture = Picture::from_grey(32, 32, grey).expect("build");
        let before = average(gray(&picture));
        let mut dithered = picture.clone();
        dithered.dither(PANEL_GREYS).expect("dither Gray8");
        let after = average(gray(&dithered));
        assert!(
            (before - after).abs() < 3.0,
            "brightness moved from {before} to {after}"
        );
    }

    #[test]
    fn one_grey_is_not_a_picture_so_the_floor_is_two() {
        let mut picture = Picture::from_grey(4, 4, vec![90; 16]).expect("build");
        picture.dither(0).expect("dither Gray8");
        for value in gray(&picture) {
            assert!(
                *value == 0 || *value == 255,
                "{value} is not black or white"
            );
        }
    }

    fn average(values: &[u8]) -> f64 {
        let count = u32::try_from(values.len()).expect("a short fixture");
        values.iter().map(|value| f64::from(*value)).sum::<f64>() / f64::from(count)
    }
}
