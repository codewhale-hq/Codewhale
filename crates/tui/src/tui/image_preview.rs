//! Terminal-cell thumbnails for attached images.
//!
//! The composer preview is drawn with ordinary cells, not a graphics
//! protocol: one cell holds two vertical pixels, `鈻€` with the upper pixel as
//! foreground and the lower as background. Every truecolor terminal can do
//! that 鈥?Windows Terminal, iTerm2, Terminal.app, GNOME Terminal, Konsole,
//! WezTerm 鈥?while kitty graphics and sixel cannot be assumed (and are not
//! available at all in Windows Terminal). `pet_watch::graphics` owns the
//! kitty path for the animation; this module is the portable one.
//!
//! Decoding is off-thread. A screenshot is megabytes of compressed data and
//! tens of milliseconds of CPU; doing that inline would stall the frame that
//! just received the paste. [`ThumbnailCache::request`] starts the decode and
//! [`ThumbnailCache::poll`] collects it on a later frame, so the composer
//! paints immediately and the picture appears when it is ready.
//!
//! Known limitation: only the first frame of an animated GIF is shown, and a
//! picture whose bytes are not valid PNG/JPEG/GIF/WebP simply has no preview
//! (the attachment itself still fails loudly at send time, where
//! `image_attach` validates it).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::SystemTime;

use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageReader};

/// Longest edge of the preview, in columns. Chosen so a landscape screenshot
/// stays a recognisable thumbnail without pushing the transcript away.
pub const PREVIEW_MAX_COLS: u16 = 40;

/// Longest edge of the preview, in rows. Eight rows is 16 pixel rows: enough
/// to tell a screenshot from a photo, small enough to leave the composer and
/// transcript room on an 24-row terminal.
pub const PREVIEW_MAX_ROWS: u16 = 8;

/// Decode budget for the background worker. A pasted screenshot is normally
/// well under this; anything larger is refused rather than decoded into
/// memory that the preview does not need anyway.
const PREVIEW_SOURCE_LIMIT: u64 = 32 * 1024 * 1024;

/// One cell of the preview: its upper and lower pixel, each absent when that
/// half is fully transparent. Absence is what lets the renderer keep the
/// theme's own background behind a transparent logo instead of painting it
/// black.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbnailCell {
    pub upper: Option<[u8; 3]>,
    pub lower: Option<[u8; 3]>,
}

/// A decoded, cell-sized preview ready to paint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub cols: u16,
    pub rows: u16,
    /// Row-major, `cols * rows` cells.
    pub cells: Vec<ThumbnailCell>,
}

impl Thumbnail {
    #[must_use]
    pub fn cell(&self, col: u16, row: u16) -> Option<&ThumbnailCell> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        self.cells
            .get(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }
}

/// The `(cols, rows)` box an image of `width 脳 height` fills.
///
/// A cell is half as wide as it is tall, so `cols` columns and `rows` rows
/// cover a `cols 脳 2路rows` pixel grid, and `cols / (2路rows)` must equal the
/// image's own aspect ratio. Reducing one axis and growing the other keeps
/// that ratio without letterboxing.
#[must_use]
pub fn thumbnail_size(width: u32, height: u32, max_cols: u16, max_rows: u16) -> (u16, u16) {
    if width == 0 || height == 0 || max_cols == 0 || max_rows == 0 {
        return (0, 0);
    }
    let aspect = f64::from(width) / f64::from(height);
    let rows_at_max = f64::from(max_cols) / (2.0 * aspect);
    let (cols, rows) = if rows_at_max >= f64::from(max_rows) {
        // Max rows binds: use every row and widen to keep the ratio.
        let cols = (2.0 * aspect * f64::from(max_rows)).round().max(1.0);
        (cols, f64::from(max_rows))
    } else {
        // Max columns binds: use every column and shorten the height.
        (f64::from(max_cols), rows_at_max.round().max(1.0))
    };
    (
        u16::try_from(cols as u32)
            .unwrap_or(max_cols)
            .min(max_cols)
            .max(1),
        u16::try_from(rows as u32)
            .unwrap_or(max_rows)
            .min(max_rows)
            .max(1),
    )
}

/// Fit `image` into `max_cols 脳 max_rows` cells and read every pixel pair.
#[must_use]
pub fn thumbnail_from_image(image: &DynamicImage, max_cols: u16, max_rows: u16) -> Thumbnail {
    let (width, height) = image.dimensions();
    let (cols, rows) = thumbnail_size(width, height, max_cols, max_rows);
    if cols == 0 || rows == 0 {
        return Thumbnail {
            cols: 0,
            rows: 0,
            cells: Vec::new(),
        };
    }
    // The sampling grid: one column per cell, two pixel rows per cell row.
    let grid = image
        .resize_exact(u32::from(cols), u32::from(rows) * 2, FilterType::Triangle)
        .to_rgba8();
    let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));
    for row in 0..rows {
        for col in 0..cols {
            let upper = grid.get_pixel(u32::from(col), u32::from(row) * 2);
            let lower = grid.get_pixel(u32::from(col), u32::from(row) * 2 + 1);
            cells.push(ThumbnailCell {
                upper: opaque_rgb(upper.0),
                lower: opaque_rgb(lower.0),
            });
        }
    }
    Thumbnail { cols, rows, cells }
}

/// Fully transparent pixels are absent; anything else is its own RGB. A
/// half-transparent pixel is composited over the terminal's background by
/// simply dropping its alpha 鈥?the preview is an identification aid, not a
/// colour-accurate rendition.
fn opaque_rgb(pixel: [u8; 4]) -> Option<[u8; 3]> {
    (pixel[3] >= 8).then_some([pixel[0], pixel[1], pixel[2]])
}

/// One cached preview, keyed by the file it came from.
#[derive(Debug, Clone)]
struct Entry {
    stamp: (SystemTime, u64),
    thumbnail: Thumbnail,
}

/// Decoded previews plus the worker that produces them.
#[derive(Debug)]
pub struct ThumbnailCache {
    entries: HashMap<PathBuf, Entry>,
    pending: HashMap<PathBuf, (SystemTime, u64)>,
    /// The attachment list the cache was last reconciled against, so the
    /// per-frame `track` call compares vectors instead of stating files.
    live: Vec<PathBuf>,
    tx: Sender<(PathBuf, (SystemTime, u64), Option<Thumbnail>)>,
    rx: Receiver<(PathBuf, (SystemTime, u64), Option<Thumbnail>)>,
}

impl Default for ThumbnailCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ThumbnailCache {
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            entries: HashMap::new(),
            pending: HashMap::new(),
            live: Vec::new(),
            tx,
            rx,
        }
    }

    /// The cached preview for `path`, if one is ready.
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<&Thumbnail> {
        self.entries.get(path).map(|entry| &entry.thumbnail)
    }

    /// Ask for `path` to be decoded, unless it is already cached or in
    /// flight. Returns immediately; the worker holds the decode.
    pub fn request(&mut self, path: &Path) {
        if self.pending.contains_key(path) {
            return;
        }
        let Ok(metadata) = std::fs::metadata(path) else {
            return;
        };
        if !metadata.is_file() || metadata.len() > PREVIEW_SOURCE_LIMIT {
            return;
        }
        let Ok(modified) = metadata.modified() else {
            return;
        };
        let stamp = (modified, metadata.len());
        if self
            .entries
            .get(path)
            .is_some_and(|entry| entry.stamp == stamp)
        {
            return;
        }
        self.pending.insert(path.to_path_buf(), stamp);
        let path = path.to_path_buf();
        let tx = self.tx.clone();
        // A fresh thread per decode keeps the frame loop free and dies with
        // the request; there is never more than one per attached image.
        let _ = std::thread::Builder::new()
            .name("image-preview-decode".to_string())
            .spawn(move || {
                let thumbnail = decode_thumbnail(&path, PREVIEW_MAX_COLS, PREVIEW_MAX_ROWS);
                let _ = tx.send((path, stamp, thumbnail));
            });
    }

    /// Keep the cache in step with what the composer shows, without touching
    /// the filesystem on an unchanged draft.
    ///
    /// The render loop calls this every frame. It only compares the attachment
    /// list, so a fast typist pays one vector comparison instead of one stat
    /// per frame (#6149: blocking work never runs inline on the paint path).
    pub fn track(&mut self, live: &[PathBuf]) {
        if self.live == live {
            return;
        }
        self.retain_only(live);
        for path in live {
            self.request(path);
        }
        self.live = live.to_vec();
    }

    /// Collect finished decodes. Called once per frame before painting.
    pub fn poll(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok((path, stamp, thumbnail)) => {
                    // A decode the draft has already dropped (the attachment
                    // was removed while it ran) must not reappear in the
                    // cache: `retain_only` cleared its pending entry.
                    if self.pending.remove(&path).is_none() {
                        continue;
                    }
                    if let Some(thumbnail) = thumbnail {
                        self.entries.insert(path, Entry { stamp, thumbnail });
                    } else {
                        self.entries.remove(&path);
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    /// Drop previews nothing refers to any more, so a long session does not
    /// retain every image the user ever attached.
    fn retain_only(&mut self, live: &[PathBuf]) {
        self.entries.retain(|path, _| live.contains(path));
        self.pending.retain(|path, _| live.contains(path));
    }
}

fn decode_thumbnail(path: &Path, max_cols: u16, max_rows: u16) -> Option<Thumbnail> {
    let reader = ImageReader::open(path).ok()?;
    let mut reader = reader.with_guessed_format().ok()?;
    // The preview never needs the full-resolution decode; bounding it keeps a
    // huge screenshot from allocating its full pixel buffer for a 40脳16 box.
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(64 * 1024 * 1024);
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    Some(thumbnail_from_image(&image, max_cols, max_rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_of(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([x as u8, y as u8, 128, 255])
        }))
    }

    #[test]
    fn wide_image_uses_full_height_and_keeps_its_ratio() {
        // 1200脳848 is the manga page the feature was requested with. The row
        // cap binds first, so the width follows from the aspect ratio.
        let (cols, rows) = thumbnail_size(1200, 848, 40, 8);
        assert_eq!(rows, 8);
        let ratio = f64::from(cols) / (2.0 * f64::from(rows));
        assert!(
            (ratio - 1200.0 / 848.0).abs() < 0.05,
            "cols={cols} rows={rows} ratio={ratio}"
        );
    }

    #[test]
    fn tall_image_uses_full_height_and_narrows_width() {
        let (cols, rows) = thumbnail_size(600, 1600, 40, 8);
        assert_eq!(rows, 8);
        assert!((5..=8).contains(&cols), "cols={cols}");
    }

    #[test]
    fn square_image_keeps_its_ratio() {
        let (cols, rows) = thumbnail_size(1024, 1024, 40, 8);
        assert_eq!(rows, 8);
        // cols / (2*rows) = 1 for a square.
        assert_eq!(cols, 16);
    }

    #[test]
    fn degenerate_sizes_are_zero_not_a_panic() {
        assert_eq!(thumbnail_size(0, 100, 40, 8), (0, 0));
        assert_eq!(thumbnail_size(100, 0, 40, 8), (0, 0));
        assert_eq!(thumbnail_size(100, 100, 0, 8), (0, 0));
        assert_eq!(thumbnail_size(100, 100, 40, 0), (0, 0));
    }

    #[test]
    fn thumbnail_covers_the_box_and_reads_both_halves() {
        let thumb = thumbnail_from_image(&image_of(80, 40), 40, 8);
        assert_eq!(
            usize::from(thumb.cols) * usize::from(thumb.rows),
            thumb.cells.len()
        );
        assert!(thumb.cells.iter().any(|cell| cell.upper.is_some()));
        assert!(thumb.cells.iter().any(|cell| cell.lower.is_some()));
        assert!(thumb.cell(0, 0).is_some());
        assert!(thumb.cell(thumb.cols, 0).is_none());
    }

    #[test]
    fn transparent_pixels_stay_absent_so_the_theme_shows_through() {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            8,
            8,
            image::Rgba([0, 0, 0, 0]),
        ));
        let thumb = thumbnail_from_image(&image, 8, 4);
        assert!(
            thumb
                .cells
                .iter()
                .all(|cell| cell.upper.is_none() && cell.lower.is_none()),
            "{:?}",
            thumb.cells
        );
    }

    #[test]
    fn cache_returns_nothing_until_the_decode_finishes_then_reuses_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        image_of(64, 32)
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        let mut cache = ThumbnailCache::new();
        // Not requested yet.
        assert!(cache.get(&path).is_none());
        cache.request(&path);
        // Poll until the worker publishes, bounded so a hang fails the test.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            cache.poll();
            if cache.get(&path).is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "decode did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let first = cache.get(&path).unwrap().clone();
        cache.request(&path);
        cache.poll();
        assert_eq!(cache.get(&path).unwrap(), &first);
        assert!(cache.pending.is_empty());
    }

    #[test]
    fn track_drops_previews_the_composer_no_longer_shows() {
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("kept.png");
        let dropped = dir.path().join("dropped.png");
        image_of(16, 16)
            .save_with_format(&kept, image::ImageFormat::Png)
            .unwrap();
        image_of(16, 16)
            .save_with_format(&dropped, image::ImageFormat::Png)
            .unwrap();
        let mut cache = ThumbnailCache::new();
        cache.request(&kept);
        cache.request(&dropped);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while cache.entries.len() < 2 {
            cache.poll();
            assert!(
                std::time::Instant::now() < deadline,
                "decodes did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        cache.track(std::slice::from_ref(&kept));
        assert!(cache.get(&kept).is_some());
        assert!(cache.get(&dropped).is_none());
    }

    #[test]
    fn missing_and_non_image_files_never_produce_a_preview() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, b"not an image").unwrap();
        let mut cache = ThumbnailCache::new();
        cache.request(&dir.path().join("absent.png"));
        cache.request(&text);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            cache.poll();
            if cache.pending.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "decode did not settle"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(cache.get(&text).is_none());
    }
}
