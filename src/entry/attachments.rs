use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use image::codecs::jpeg::{JpegDecoder, JpegEncoder};
use image::{DynamicImage, GenericImageView, ImageDecoder, Rgb, RgbImage};
use lopdf::{Dictionary as PdfDictionary, Document as PdfDocument, Object as PdfObject};
use md5::{Digest as Md5Digest, Md5};
use rsa::rand_core::{OsRng, RngCore};
use sha2::Sha256;

use crate::util::normalize_entry_id;

const MAX_ATTACHMENT_FILE_SIZE_BYTES: u64 = 500 * 1024 * 1024;
pub const MAX_ATTACHMENTS_PER_ENTRY: usize = 1000;
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_TOTAL_PIXELS: u64 = 67_108_864; // 8192x8192
const MAX_VIDEO_THUMBNAIL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PDF_DECOMPRESSED_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const THUMBNAIL_MAX_DIMENSION: u32 = 640;
const THUMBNAIL_JPEG_QUALITY: u8 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    Image,
    Video,
    Audio,
    PdfAttachment,
}

impl MediaType {
    pub fn api_type_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::PdfAttachment => "pdfAttachment",
        }
    }

    pub fn placeholder_path_segment(self) -> Option<&'static str> {
        match self {
            Self::Image => None,
            _ => Some(self.api_type_str()),
        }
    }

    fn default_mime(self) -> &'static str {
        match self {
            Self::Image => "image/jpeg",
            Self::Video => "video/mp4",
            Self::Audio => "audio/mp4",
            Self::PdfAttachment => "application/pdf",
        }
    }

    pub fn as_rich_text_embed_type(self) -> &'static str {
        match self {
            // Web editor embedded object type for still images is "photo".
            Self::Image => "photo",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::PdfAttachment => "pdfAttachment",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AttachmentThumbnail {
    pub content_type: String,
    pub md5: String,
    pub width: i64,
    pub height: i64,
    pub file_size_bytes: i64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct StagedAttachment {
    pub file_path: String,
    pub moment_id: String,
    pub moment_type: MediaType,
    pub content_type: String,
    pub file_size_bytes: i64,
    pub md5_body: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_seconds: Option<f64>,
    pub thumbnail: Option<AttachmentThumbnail>,
}

#[derive(Debug, Clone)]
pub struct NewMoment {
    pub id: String,
    pub moment_type: MediaType,
    pub content_type: String,
    pub md5_body: String,
    pub file_size_bytes: i64,
    pub pdf_name: Option<String>,
    pub thumbnail: Option<NewMomentThumbnail>,
}

#[derive(Debug, Clone)]
pub struct NewMomentThumbnail {
    pub content_type: String,
    pub md5: String,
    pub width: i64,
    pub height: i64,
    pub file_size_bytes: i64,
}

#[derive(Debug, Clone, Default)]
struct AttachmentDerivedMetadata {
    width: Option<i64>,
    height: Option<i64>,
    duration_seconds: Option<f64>,
    thumbnail: Option<AttachmentThumbnail>,
}

#[derive(Debug, Clone, Copy)]
struct VideoProbeMetadata {
    width: Option<u32>,
    height: Option<u32>,
    duration_seconds: Option<f64>,
}

pub fn build_new_moments(staged_attachments: &[StagedAttachment]) -> Vec<NewMoment> {
    staged_attachments
        .iter()
        .map(|attachment| NewMoment {
            id: attachment.moment_id.clone(),
            moment_type: attachment.moment_type,
            content_type: attachment.content_type.clone(),
            md5_body: attachment.md5_body.clone(),
            file_size_bytes: attachment.file_size_bytes,
            pdf_name: derive_pdf_name_for_moment(attachment),
            thumbnail: attachment
                .thumbnail
                .as_ref()
                .map(|thumbnail| NewMomentThumbnail {
                    content_type: thumbnail.content_type.clone(),
                    md5: thumbnail.md5.clone(),
                    width: thumbnail.width,
                    height: thumbnail.height,
                    file_size_bytes: thumbnail.file_size_bytes,
                }),
        })
        .collect()
}

pub fn stage_attachments(
    attachment_paths: &[PathBuf],
    attachment_types: &[MediaType],
) -> Result<Vec<StagedAttachment>> {
    let mut staged = Vec::with_capacity(attachment_paths.len());
    for (idx, path) in attachment_paths.iter().enumerate() {
        let metadata = fs::metadata(path).with_context(|| {
            format!(
                "failed to read attachment metadata for '{}'",
                path.display()
            )
        })?;
        if !metadata.is_file() {
            bail!("attachment '{}' is not a file", path.display());
        }
        if metadata.len() > MAX_ATTACHMENT_FILE_SIZE_BYTES {
            bail!(
                "attachment '{}' exceeds max size of {} bytes",
                path.display(),
                MAX_ATTACHMENT_FILE_SIZE_BYTES
            );
        }
        let md5_body = compute_md5_for_file(path)?;
        let moment_type = attachment_types
            .get(idx)
            .copied()
            .unwrap_or_else(|| infer_attachment_type(path));
        let content_type = infer_content_type(path, moment_type);
        let derived = derive_attachment_metadata(path, moment_type, &content_type);
        staged.push(StagedAttachment {
            file_path: canonical_attachment_path(path)?,
            moment_id: normalize_or_generate_entry_id(None)?,
            moment_type,
            content_type,
            file_size_bytes: i64::try_from(metadata.len())
                .context("attachment file size overflowed i64 range")?,
            md5_body,
            width: derived.width,
            height: derived.height,
            duration_seconds: derived.duration_seconds,
            thumbnail: derived.thumbnail,
        });
    }
    Ok(staged)
}

pub fn normalize_or_generate_entry_id(entry_id: Option<&str>) -> Result<String> {
    if let Some(id) = entry_id {
        let normalized = normalize_entry_id(id);
        if normalized.is_empty() {
            bail!("entry_id cannot be empty");
        }
        return Ok(normalized);
    }
    let mut hasher = Sha256::new();
    hasher.update(now_epoch_ms().to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    let mut nonce = [0_u8; 16];
    let mut rng = OsRng;
    rng.fill_bytes(&mut nonce);
    hasher.update(nonce);
    let digest = hasher.finalize();
    let hex = format!("{:x}", digest);
    Ok(hex[0..32].to_ascii_uppercase())
}

pub fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn derive_pdf_name_for_moment(attachment: &StagedAttachment) -> Option<String> {
    if attachment.moment_type != MediaType::PdfAttachment {
        return None;
    }
    let path = Path::new(&attachment.file_path);
    let stem = path.file_stem()?.to_str()?.trim();
    if stem.is_empty() {
        return None;
    }
    Some(stem.to_owned())
}

fn derive_attachment_metadata(
    path: &Path,
    moment_type: MediaType,
    content_type: &str,
) -> AttachmentDerivedMetadata {
    match moment_type {
        MediaType::Image => match derive_image_metadata(path) {
            Ok(metadata) => metadata,
            Err(err) => {
                eprintln!(
                    "warning: failed to derive image thumbnail for '{}': {err:#}",
                    path.display()
                );
                AttachmentDerivedMetadata::default()
            }
        },
        MediaType::Video => match derive_video_metadata(path, content_type) {
            Ok(metadata) => metadata,
            Err(err) => {
                eprintln!(
                    "warning: failed to derive video thumbnail for '{}': {err:#}",
                    path.display()
                );
                AttachmentDerivedMetadata::default()
            }
        },
        MediaType::Audio => AttachmentDerivedMetadata::default(),
        MediaType::PdfAttachment => match derive_pdf_metadata(path, content_type) {
            Ok(metadata) => metadata,
            Err(err) => {
                eprintln!(
                    "warning: failed to derive PDF thumbnail for '{}': {err:#}",
                    path.display()
                );
                AttachmentDerivedMetadata::default()
            }
        },
    }
}

fn derive_video_metadata(path: &Path, _content_type: &str) -> Result<AttachmentDerivedMetadata> {
    let scaled_max = THUMBNAIL_MAX_DIMENSION.to_string();
    let ffmpeg_quality = ffmpeg_mjpeg_quality_for_thumbnail();
    let probe = derive_video_probe_metadata(path);
    let scale_filter = format!(
        "scale={0}:{0}:force_original_aspect_ratio=decrease",
        THUMBNAIL_MAX_DIMENSION
    );
    let thumbnail_bytes = extract_video_thumbnail_bytes(
        path,
        &scale_filter,
        ffmpeg_quality,
        MAX_VIDEO_THUMBNAIL_BYTES,
    )?;
    let decoder = JpegDecoder::new(Cursor::new(&thumbnail_bytes)).with_context(|| {
        format!(
            "failed to parse ffmpeg thumbnail JPEG header for '{}'",
            path.display()
        )
    })?;
    let (thumbnail_width, thumbnail_height) = decoder.dimensions();
    if thumbnail_width == 0 || thumbnail_height == 0 {
        bail!("ffmpeg produced invalid thumbnail dimensions 0x0");
    }
    if thumbnail_width > THUMBNAIL_MAX_DIMENSION || thumbnail_height > THUMBNAIL_MAX_DIMENSION {
        bail!(
            "ffmpeg returned oversized frame {thumbnail_width}x{thumbnail_height} despite scale={scaled_max}"
        );
    }
    let thumbnail_md5 = format!("{:x}", Md5::digest(&thumbnail_bytes));
    let thumbnail_size =
        i64::try_from(thumbnail_bytes.len()).context("thumbnail file size overflowed i64 range")?;

    Ok(AttachmentDerivedMetadata {
        width: probe.width.map(i64::from),
        height: probe.height.map(i64::from),
        duration_seconds: probe.duration_seconds,
        thumbnail: Some(AttachmentThumbnail {
            content_type: "image/jpeg".to_owned(),
            md5: thumbnail_md5,
            width: i64::from(thumbnail_width),
            height: i64::from(thumbnail_height),
            file_size_bytes: thumbnail_size,
            bytes: thumbnail_bytes,
        }),
    })
}

fn ffmpeg_mjpeg_quality_for_thumbnail() -> u8 {
    // ffmpeg MJPEG `-q:v` is inverse quality: 2 (best) .. 31 (worst).
    // Map our shared JPEG quality scale (1..=100, higher is better) into that range.
    let clamped = THUMBNAIL_JPEG_QUALITY.clamp(1, 100);
    let inverted = 100_u32.saturating_sub(u32::from(clamped));
    let q = 2_u32 + (inverted * 29 + 50) / 100;
    q.clamp(2, 31) as u8
}

fn extract_video_thumbnail_bytes(
    path: &Path,
    scale_filter: &str,
    ffmpeg_quality: u8,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    let mut nonce = [0_u8; 8];
    let mut rng = OsRng;
    rng.fill_bytes(&mut nonce);
    let nonce_hex = nonce
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let temp_path = std::env::temp_dir().join(format!(
        "dayone-cli-thumb-{}-{}-{}.jpg",
        std::process::id(),
        now_epoch_ms(),
        nonce_hex
    ));

    let result = (|| -> Result<Vec<u8>> {
        let ffmpeg = Command::new("ffmpeg")
            .arg("-nostdin")
            .arg("-y")
            .arg("-v")
            .arg("error")
            .arg("-i")
            .arg(path)
            .arg("-vf")
            .arg(scale_filter)
            .arg("-frames:v")
            .arg("1")
            .arg("-vcodec")
            .arg("mjpeg")
            .arg("-q:v")
            .arg(ffmpeg_quality.to_string())
            .arg(&temp_path)
            .stdin(Stdio::null())
            .output()
            .with_context(|| "failed to execute ffmpeg for video thumbnail generation")?;
        if !ffmpeg.status.success() {
            let stderr = String::from_utf8_lossy(&ffmpeg.stderr).trim().to_owned();
            bail!(
                "ffmpeg failed to extract thumbnail frame: {}",
                if stderr.is_empty() {
                    "unknown error"
                } else {
                    stderr.as_str()
                }
            );
        }

        let metadata = fs::metadata(&temp_path)
            .with_context(|| "failed to read ffmpeg thumbnail metadata from temp file")?;
        if metadata.len() == 0 {
            bail!("ffmpeg produced an empty thumbnail frame");
        }
        if metadata.len() > max_bytes {
            bail!(
                "ffmpeg produced oversized thumbnail frame ({} bytes > {} bytes)",
                metadata.len(),
                max_bytes
            );
        }
        fs::read(&temp_path).with_context(|| "failed to read ffmpeg thumbnail temp file")
    })();

    let _ = fs::remove_file(&temp_path);
    result
}

fn derive_video_probe_metadata(path: &Path) -> VideoProbeMetadata {
    let ffprobe = Command::new("ffprobe")
        .arg("-v")
        .arg("error")
        .arg("-select_streams")
        .arg("v:0")
        .arg("-show_entries")
        .arg("stream=width,height:format=duration")
        .arg("-of")
        .arg("default=nokey=0:noprint_wrappers=1")
        .arg("-i")
        .arg(path)
        .stdin(Stdio::null())
        .output();
    let Ok(ffprobe) = ffprobe else {
        return VideoProbeMetadata {
            width: None,
            height: None,
            duration_seconds: None,
        };
    };
    if !ffprobe.status.success() {
        return VideoProbeMetadata {
            width: None,
            height: None,
            duration_seconds: None,
        };
    }
    let raw = String::from_utf8_lossy(&ffprobe.stdout);
    let mut width = None;
    let mut height = None;
    let mut duration_seconds = None;
    for line in raw.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "width" => {
                width = value.parse::<u32>().ok().filter(|parsed| *parsed > 0);
            }
            "height" => {
                height = value.parse::<u32>().ok().filter(|parsed| *parsed > 0);
            }
            "duration" => {
                let parsed = value.parse::<f64>().ok();
                duration_seconds = parsed
                    .and_then(|seconds| (seconds.is_finite() && seconds > 0.0).then_some(seconds));
            }
            _ => {}
        }
    }
    VideoProbeMetadata {
        width,
        height,
        duration_seconds,
    }
}

fn derive_pdf_metadata(path: &Path, _content_type: &str) -> Result<AttachmentDerivedMetadata> {
    let document = PdfDocument::load(path)
        .with_context(|| format!("failed to parse PDF attachment '{}'", path.display()))?;
    let (page_width, page_height) = derive_pdf_page_dimensions(&document)?;
    let thumbnail_source_image = extract_pdf_first_page_image(&document)?
        .unwrap_or_else(|| render_pdf_thumbnail_placeholder(page_width, page_height));
    let thumbnail_image =
        thumbnail_source_image.thumbnail(THUMBNAIL_MAX_DIMENSION, THUMBNAIL_MAX_DIMENSION);
    let thumbnail_bytes = encode_thumbnail_jpeg(&thumbnail_image)
        .with_context(|| format!("failed to encode PDF thumbnail for '{}'", path.display()))?;
    let thumbnail_md5 = format!("{:x}", Md5::digest(&thumbnail_bytes));
    let thumbnail_size =
        i64::try_from(thumbnail_bytes.len()).context("thumbnail file size overflowed i64 range")?;
    let (thumbnail_width, thumbnail_height) = thumbnail_image.dimensions();

    Ok(AttachmentDerivedMetadata {
        width: Some(i64::from(page_width)),
        height: Some(i64::from(page_height)),
        duration_seconds: None,
        thumbnail: Some(AttachmentThumbnail {
            content_type: "image/jpeg".to_owned(),
            md5: thumbnail_md5,
            width: i64::from(thumbnail_width),
            height: i64::from(thumbnail_height),
            file_size_bytes: thumbnail_size,
            bytes: thumbnail_bytes,
        }),
    })
}

fn derive_pdf_page_dimensions(document: &PdfDocument) -> Result<(u32, u32)> {
    const DEFAULT_PDF_PAGE_WIDTH: u32 = 612;
    const DEFAULT_PDF_PAGE_HEIGHT: u32 = 792;

    let page_id = document
        .get_pages()
        .into_values()
        .next()
        .context("PDF attachment has no pages")?;
    let Some(media_box_object) = resolve_inherited_page_attribute(document, page_id, b"MediaBox")?
    else {
        return Ok((DEFAULT_PDF_PAGE_WIDTH, DEFAULT_PDF_PAGE_HEIGHT));
    };
    let media_box_array = resolve_pdf_object(document, media_box_object)?
        .as_array()
        .context("MediaBox should be an array")?;
    if media_box_array.len() < 4 {
        bail!("MediaBox should contain four numbers");
    }

    let llx = pdf_number(
        resolve_pdf_object(document, &media_box_array[0]).context("invalid MediaBox llx")?,
    )
    .context("MediaBox llx should be numeric")?;
    let lly = pdf_number(
        resolve_pdf_object(document, &media_box_array[1]).context("invalid MediaBox lly")?,
    )
    .context("MediaBox lly should be numeric")?;
    let urx = pdf_number(
        resolve_pdf_object(document, &media_box_array[2]).context("invalid MediaBox urx")?,
    )
    .context("MediaBox urx should be numeric")?;
    let ury = pdf_number(
        resolve_pdf_object(document, &media_box_array[3]).context("invalid MediaBox ury")?,
    )
    .context("MediaBox ury should be numeric")?;

    let width = (urx - llx).abs();
    let height = (ury - lly).abs();
    if width <= 0.0 || height <= 0.0 {
        bail!("PDF page has invalid dimensions");
    }

    Ok((
        width.round().clamp(1.0, f64::from(u32::MAX)) as u32,
        height.round().clamp(1.0, f64::from(u32::MAX)) as u32,
    ))
}

fn extract_pdf_first_page_image(document: &PdfDocument) -> Result<Option<DynamicImage>> {
    let page_id = match document.get_pages().into_values().next() {
        Some(id) => id,
        None => return Ok(None),
    };
    let resources = resolve_page_resources(document, page_id)?;
    let Some(xobjects) = resources
        .as_ref()
        .and_then(|dict| dict.get(b"XObject").ok())
    else {
        return Ok(None);
    };
    let xobject_dictionary = resolve_pdf_object(document, xobjects)?
        .as_dict()
        .context("XObject should be a dictionary")?;

    for (_, xobject_ref) in xobject_dictionary {
        let object = resolve_pdf_object(document, xobject_ref)?;
        let Ok(stream) = object.as_stream() else {
            continue;
        };
        if !is_pdf_image_xobject(&stream.dict) {
            continue;
        }
        let Some((width, height)) = pdf_image_stream_dimensions(document, &stream.dict)? else {
            continue;
        };
        if validate_image_dimensions(width, height).is_err() {
            continue;
        }

        if let Ok(decoded) = image::load_from_memory(&stream.content) {
            return Ok(Some(decoded));
        }
        if pdf_stream_has_encoded_image_filter(&stream.dict) {
            if stream.content.len() > MAX_PDF_DECOMPRESSED_IMAGE_BYTES {
                continue;
            }
            let Ok(decompressed) = stream.decompressed_content() else {
                continue;
            };
            if decompressed.len() > MAX_PDF_DECOMPRESSED_IMAGE_BYTES {
                continue;
            }
            if let Ok(decoded) = image::load_from_memory(&decompressed) {
                return Ok(Some(decoded));
            }
        }
    }

    Ok(None)
}

fn pdf_image_stream_dimensions(
    document: &PdfDocument,
    dictionary: &PdfDictionary,
) -> Result<Option<(u32, u32)>> {
    let Some(width_raw) = dictionary.get(b"Width").ok() else {
        return Ok(None);
    };
    let Some(height_raw) = dictionary.get(b"Height").ok() else {
        return Ok(None);
    };
    let width_obj = resolve_pdf_object(document, width_raw)?;
    let height_obj = resolve_pdf_object(document, height_raw)?;
    let Some(width_f64) = pdf_number(width_obj) else {
        return Ok(None);
    };
    let Some(height_f64) = pdf_number(height_obj) else {
        return Ok(None);
    };
    if !width_f64.is_finite() || !height_f64.is_finite() || width_f64 <= 0.0 || height_f64 <= 0.0 {
        return Ok(None);
    }
    let width = width_f64.round().clamp(1.0, f64::from(u32::MAX)) as u32;
    let height = height_f64.round().clamp(1.0, f64::from(u32::MAX)) as u32;
    Ok(Some((width, height)))
}

fn resolve_page_resources(
    document: &PdfDocument,
    page_id: lopdf::ObjectId,
) -> Result<Option<&PdfDictionary>> {
    let Some(resources) = resolve_inherited_page_attribute(document, page_id, b"Resources")? else {
        return Ok(None);
    };
    let resources_object = resolve_pdf_object(document, resources)?;
    let resources_dictionary = resources_object
        .as_dict()
        .context("Resources should be a dictionary")?;
    Ok(Some(resources_dictionary))
}

fn resolve_inherited_page_attribute<'a>(
    document: &'a PdfDocument,
    start_page_id: lopdf::ObjectId,
    attribute: &[u8],
) -> Result<Option<&'a PdfObject>> {
    let mut current_id = start_page_id;
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current_id) {
            bail!("detected cycle while resolving inherited page attributes");
        }
        let dictionary = document
            .get_dictionary(current_id)
            .with_context(|| format!("failed to read PDF page/tree dictionary '{current_id:?}'"))?;
        if let Ok(value) = dictionary.get(attribute) {
            return Ok(Some(value));
        }
        let Some(parent_ref) = dictionary.get(b"Parent").ok() else {
            return Ok(None);
        };
        let PdfObject::Reference(parent_id) = parent_ref else {
            return Ok(None);
        };
        current_id = *parent_id;
    }
}

fn is_pdf_image_xobject(dictionary: &PdfDictionary) -> bool {
    let is_xobject = dictionary
        .get(b"Type")
        .ok()
        .and_then(pdf_name)
        .map(|name| name == b"XObject")
        .unwrap_or(false);
    let is_image = dictionary
        .get(b"Subtype")
        .ok()
        .and_then(pdf_name)
        .map(|name| name == b"Image")
        .unwrap_or(false);
    is_xobject && is_image
}

fn pdf_stream_has_encoded_image_filter(dictionary: &PdfDictionary) -> bool {
    const IMAGE_ENCODED_FILTERS: [&[u8]; 2] = [b"DCTDecode", b"JPXDecode"];
    match dictionary.get(b"Filter").ok() {
        Some(PdfObject::Name(name)) => IMAGE_ENCODED_FILTERS.contains(&name.as_slice()),
        Some(PdfObject::Array(items)) => items.iter().any(|item| {
            matches!(
                item,
                PdfObject::Name(name)
                    if IMAGE_ENCODED_FILTERS.contains(&name.as_slice())
            )
        }),
        _ => false,
    }
}

fn pdf_name(object: &PdfObject) -> Option<&[u8]> {
    match object {
        PdfObject::Name(name) => Some(name.as_slice()),
        _ => None,
    }
}

fn resolve_pdf_object<'a>(
    document: &'a PdfDocument,
    object: &'a PdfObject,
) -> Result<&'a PdfObject> {
    match object {
        PdfObject::Reference(reference) => document
            .get_object(*reference)
            .with_context(|| format!("failed to resolve referenced PDF object '{reference:?}'")),
        _ => Ok(object),
    }
}

fn pdf_number(object: &PdfObject) -> Option<f64> {
    match object {
        PdfObject::Integer(value) => Some(*value as f64),
        PdfObject::Real(value) => Some((*value).into()),
        _ => None,
    }
}

fn render_pdf_thumbnail_placeholder(page_width: u32, page_height: u32) -> DynamicImage {
    let (thumbnail_width, thumbnail_height) =
        scale_dimensions_within_bounds(page_width, page_height, THUMBNAIL_MAX_DIMENSION);
    let mut image = RgbImage::from_pixel(thumbnail_width, thumbnail_height, Rgb([248, 248, 246]));

    // Draw a subtle border to make the PDF card visible on white backgrounds.
    let border_color = Rgb([210, 210, 210]);
    for x in 0..thumbnail_width {
        image.put_pixel(x, 0, border_color);
        image.put_pixel(x, thumbnail_height - 1, border_color);
    }
    for y in 0..thumbnail_height {
        image.put_pixel(0, y, border_color);
        image.put_pixel(thumbnail_width - 1, y, border_color);
    }

    DynamicImage::ImageRgb8(image)
}

fn scale_dimensions_within_bounds(width: u32, height: u32, max_dimension: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (max_dimension, max_dimension);
    }
    if width <= max_dimension && height <= max_dimension {
        return (width, height);
    }
    let scale = f64::min(
        f64::from(max_dimension) / f64::from(width),
        f64::from(max_dimension) / f64::from(height),
    );
    let scaled_width = (f64::from(width) * scale)
        .round()
        .clamp(1.0, f64::from(max_dimension)) as u32;
    let scaled_height = (f64::from(height) * scale)
        .round()
        .clamp(1.0, f64::from(max_dimension)) as u32;
    (scaled_width, scaled_height)
}

fn derive_image_metadata(path: &Path) -> Result<AttachmentDerivedMetadata> {
    let (width, height) = image::image_dimensions(path)
        .with_context(|| format!("failed to read image dimensions for '{}'", path.display()))?;
    validate_image_dimensions(width, height)?;

    // `image::open` is more tolerant across common PNG/JPEG/WebP fixtures than
    // manually guessing formats via `ImageReader`, which can fail and skip thumbs.
    let image = image::open(path)
        .with_context(|| format!("failed to decode image attachment '{}'", path.display()))?;
    let thumbnail_image = image.thumbnail(THUMBNAIL_MAX_DIMENSION, THUMBNAIL_MAX_DIMENSION);
    let thumbnail_bytes = encode_thumbnail_jpeg(&thumbnail_image)
        .with_context(|| format!("failed to encode thumbnail for '{}'", path.display()))?;
    let thumbnail_md5 = format!("{:x}", Md5::digest(&thumbnail_bytes));
    let thumbnail_size =
        i64::try_from(thumbnail_bytes.len()).context("thumbnail file size overflowed i64 range")?;
    let (thumbnail_width, thumbnail_height) = thumbnail_image.dimensions();
    Ok(AttachmentDerivedMetadata {
        width: Some(i64::from(width)),
        height: Some(i64::from(height)),
        duration_seconds: None,
        thumbnail: Some(AttachmentThumbnail {
            content_type: "image/jpeg".to_owned(),
            md5: thumbnail_md5,
            width: i64::from(thumbnail_width),
            height: i64::from(thumbnail_height),
            file_size_bytes: thumbnail_size,
            bytes: thumbnail_bytes,
        }),
    })
}

fn validate_image_dimensions(width: u32, height: u32) -> Result<()> {
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        bail!(
            "image dimensions {}x{} exceed supported maximum of {} pixels on a side",
            width,
            height,
            MAX_IMAGE_DIMENSION
        );
    }
    let total_pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .context("image dimensions overflowed total pixel computation")?;
    if total_pixels > MAX_IMAGE_TOTAL_PIXELS {
        bail!(
            "image dimensions {}x{} exceed supported maximum total pixels ({})",
            width,
            height,
            MAX_IMAGE_TOTAL_PIXELS
        );
    }
    Ok(())
}

fn encode_thumbnail_jpeg(image: &DynamicImage) -> Result<Vec<u8>> {
    let rgb = image.to_rgb8();
    let (width, height) = rgb.dimensions();
    let mut out = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut out, THUMBNAIL_JPEG_QUALITY);
    encoder.encode(&rgb, width, height, image::ColorType::Rgb8.into())?;
    Ok(out)
}

fn compute_md5_for_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("failed to open attachment for hashing '{}'", path.display()))?;
    let mut hasher = Md5::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).with_context(|| {
            format!("failed reading attachment for hashing '{}'", path.display())
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn canonical_attachment_path(path: &Path) -> Result<String> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve attachment path '{}'", path.display()))?;
    Ok(canonical.to_string_lossy().into_owned())
}

fn infer_attachment_type(path: &Path) -> MediaType {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" => MediaType::Image,
        "pdf" => MediaType::PdfAttachment,
        "m4a" | "aac" | "mp3" | "wav" | "aif" | "aiff" | "flac" | "ogg" => MediaType::Audio,
        "mp4" | "mov" | "m4v" | "webm" => MediaType::Video,
        _ => MediaType::Image,
    }
}

fn infer_content_type(path: &Path, media_type: MediaType) -> String {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let inferred = match ext.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "heic" => Some("image/heic"),
        "heif" => Some("image/heif"),
        "pdf" => Some("application/pdf"),
        "mp3" => Some("audio/mpeg"),
        "wav" => Some("audio/wav"),
        "ogg" => Some("audio/ogg"),
        "aac" => Some("audio/aac"),
        "m4a" => Some("audio/mp4"),
        "mp4" => Some("video/mp4"),
        "mov" => Some("video/quicktime"),
        "webm" => Some("video/webm"),
        _ => None,
    };
    inferred.unwrap_or(media_type.default_mime()).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file_path(suffix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("dayone-cli-entry-attachment-{nanos}.{suffix}"))
    }

    fn tiny_png_bytes() -> &'static [u8] {
        &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0xF0, 0x1F, 0x00, 0x05, 0x00, 0x01, 0xFF, 0x89, 0x99,
            0x3D, 0x1D, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ]
    }

    fn command_available(name: &str) -> bool {
        Command::new(name)
            .arg("-version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn entry_id_is_upper_hex_and_32_chars() {
        let id = normalize_or_generate_entry_id(None).expect("id should generate");
        assert_eq!(id.len(), 32);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase())
        );
    }

    #[test]
    fn accepts_non_hex_entry_id_for_updates() {
        let id = normalize_or_generate_entry_id(Some("strava-1000116162"))
            .expect("non-hex id should be allowed");
        assert_eq!(id, "strava-1000116162");
    }

    #[test]
    fn normalizes_hex_entry_id_to_uppercase() {
        let id = normalize_or_generate_entry_id(Some("a65afa22b98a0dec62eaad3cd1ccf45d"))
            .expect("hex id should normalize");
        assert_eq!(id, "A65AFA22B98A0DEC62EAAD3CD1CCF45D");
    }

    #[test]
    fn rejects_empty_entry_id() {
        let err = normalize_or_generate_entry_id(Some("   ")).expect_err("empty id should fail");
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn stage_attachments_infers_type_and_content_type() {
        let path = temp_file_path("pdf");
        {
            let mut file = fs::File::create(&path).expect("temp file should create");
            file.write_all(b"dummy pdf bytes")
                .expect("temp file should write");
        }
        let staged = stage_attachments(std::slice::from_ref(&path), &[])
            .expect("attachment staging should succeed");
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].moment_type, MediaType::PdfAttachment);
        assert_eq!(staged[0].content_type, "application/pdf");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn stage_attachments_generates_image_thumbnail_and_dimensions() {
        let path = temp_file_path("png");
        {
            let mut file = fs::File::create(&path).expect("temp file should create");
            file.write_all(tiny_png_bytes())
                .expect("temp file should write png bytes");
        }
        let staged = stage_attachments(std::slice::from_ref(&path), &[MediaType::Image])
            .expect("attachment staging should succeed");
        assert_eq!(staged.len(), 1);
        assert!(staged[0].width.unwrap_or_default() > 0);
        assert!(staged[0].height.unwrap_or_default() > 0);
        let thumbnail = staged[0]
            .thumbnail
            .as_ref()
            .expect("image thumbnail should exist");
        assert_eq!(thumbnail.content_type, "image/jpeg");
        assert!(thumbnail.width > 0);
        assert!(thumbnail.width <= i64::from(THUMBNAIL_MAX_DIMENSION));
        assert!(thumbnail.height > 0);
        assert!(thumbnail.height <= i64::from(THUMBNAIL_MAX_DIMENSION));
        assert!(!thumbnail.md5.is_empty());
        assert!(!thumbnail.bytes.is_empty());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn stage_attachments_generates_video_thumbnail_when_ffmpeg_available() {
        if !command_available("ffmpeg") {
            eprintln!("skipping test: ffmpeg not available in PATH");
            return;
        }
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("e2e/fixtures/sample-video.mp4");
        assert!(
            fixture.exists(),
            "video fixture missing at '{}'",
            fixture.display()
        );
        let staged = stage_attachments(std::slice::from_ref(&fixture), &[MediaType::Video])
            .expect("video attachment staging should succeed");
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].moment_type, MediaType::Video);
        if command_available("ffprobe") {
            assert!(staged[0].width.unwrap_or_default() > 0);
            assert!(staged[0].height.unwrap_or_default() > 0);
        }

        let thumbnail = staged[0]
            .thumbnail
            .as_ref()
            .expect("video thumbnail should exist");
        assert_eq!(thumbnail.content_type, "image/jpeg");
        assert!(thumbnail.width > 0);
        assert!(thumbnail.width <= i64::from(THUMBNAIL_MAX_DIMENSION));
        assert!(thumbnail.height > 0);
        assert!(thumbnail.height <= i64::from(THUMBNAIL_MAX_DIMENSION));
        assert!(!thumbnail.md5.is_empty());
        assert!(!thumbnail.bytes.is_empty());

        if command_available("ffprobe") {
            assert!(staged[0].duration_seconds.unwrap_or_default() > 0.0);
        }
    }

    #[test]
    fn stage_attachments_generates_pdf_thumbnail() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("e2e/fixtures/sample-pdf.pdf");
        assert!(
            fixture.exists(),
            "pdf fixture missing at '{}'",
            fixture.display()
        );
        let staged = stage_attachments(std::slice::from_ref(&fixture), &[MediaType::PdfAttachment])
            .expect("PDF attachment staging should succeed");
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].moment_type, MediaType::PdfAttachment);
        assert!(staged[0].width.unwrap_or_default() > 0);
        assert!(staged[0].height.unwrap_or_default() > 0);

        let thumbnail = staged[0]
            .thumbnail
            .as_ref()
            .expect("PDF thumbnail should exist");
        assert_eq!(thumbnail.content_type, "image/jpeg");
        assert!(thumbnail.width > 0);
        assert!(thumbnail.width <= i64::from(THUMBNAIL_MAX_DIMENSION));
        assert!(thumbnail.height > 0);
        assert!(thumbnail.height <= i64::from(THUMBNAIL_MAX_DIMENSION));
        assert!(!thumbnail.md5.is_empty());
        assert!(!thumbnail.bytes.is_empty());
    }

    #[test]
    fn validate_image_dimensions_rejects_excessive_total_pixels() {
        let err = validate_image_dimensions(16_384, 16_384)
            .expect_err("huge decoded image should be rejected");
        assert!(err.to_string().contains("supported maximum total pixels"));
    }

    #[test]
    fn ffmpeg_quality_mapping_is_in_expected_range() {
        let q = ffmpeg_mjpeg_quality_for_thumbnail();
        assert!((2..=31).contains(&q));
    }
}
