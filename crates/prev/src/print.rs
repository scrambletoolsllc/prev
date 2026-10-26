//! Printing what is not a PDF, an image or a Markdown document: its pages
//! go into a PDF in a private temporary file, which the system's print
//! dialog prints as it prints a document.

use std::ops::Range;
use std::sync::mpsc;

use prev_image::decode::Frame;
use prev_pdf::PictureDocument;
use prev_pdf::engine::Bitmap;
use prev_pdf::geometry::{Rect, Size};

use crate::i18n::Describe;
use crate::image::editor::spawn;

const LETTER: Size = Size::new(612.0, 792.0);
const A4: Size = Size::new(595.0, 842.0);
/// Regions where Letter is the usual paper.
const LETTER_REGIONS: &[&str] = &[
    "US", "CA", "MX", "PR", "PH", "CL", "CO", "VE", "GT", "CR", "PA", "DO", "SV", "NI",
];
/// A printed page's margin, in points: three quarters of an inch.
pub const MARGIN: f32 = 54.0;
/// Points a pixel of the screen's layout prints at, as CSS has it: text
/// 16 pixels tall prints at 12 points.
pub const POINTS_PER_PIXEL: f32 = 0.75;
/// Dots drawn for a pixel of the layout, for 240 dots an inch on paper.
pub const DOTS_PER_PIXEL: f32 = 2.5;
/// The longest side, in points, a PDF page may have.
const LONGEST_PAGE: f32 = 14400.0;

/// A printed page: its picture, the page's size, and where on it the
/// picture goes, in points from its bottom left.
pub struct Page {
    pub picture: Bitmap,
    pub size: Size,
    pub place: Rect,
}

/// The paper pages are made for: Letter where it is the usual paper, A4
/// elsewhere. The print dialogs fit them to the paper chosen.
pub fn paper() -> Size {
    match region() {
        Some(region) if LETTER_REGIONS.contains(&region.as_str()) => LETTER,
        _ => A4,
    }
}

/// The region of the system's paper setting, where it has one, or of its
/// language.
fn region() -> Option<String> {
    // The first one set wins, as POSIX looks them up.
    ["LC_ALL", "LC_PAPER", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .and_then(|locale| posix_region(&locale))
        .or_else(|| {
            i18n_embed::DesktopLanguageRequester::requested_languages()
                .into_iter()
                .find_map(|language| language.region.map(|region| region.as_str().to_owned()))
        })
}

/// The region of a POSIX locale such as `en_US.UTF-8` or `de_DE@euro`.
fn posix_region(locale: &str) -> Option<String> {
    let name = locale.split(['.', '@']).next()?;
    let (_, region) = name.split_once('_')?;
    Some(region.to_ascii_uppercase())
}

/// A page of `frame` alone, `width` by `height` pixels of the screen's
/// layout, the picture filling it.
pub fn picture_page(frame: Frame, width: f32, height: f32) -> Page {
    let scale = (LONGEST_PAGE / (width.max(height) * POINTS_PER_PIXEL)).min(1.0);
    let size = Size::new(
        (width * POINTS_PER_PIXEL * scale).max(1.0),
        (height * POINTS_PER_PIXEL * scale).max(1.0),
    );
    Page {
        picture: Bitmap {
            width: frame.width,
            height: frame.height,
            pixels: frame.pixels,
        },
        size,
        place: Rect::new(0.0, 0.0, size.width, size.height),
    }
}

/// How many of the first `rows` rows of `pixels`, RGBA rows `width`
/// pixels wide, best make a page, `pictures` being the rows pictures
/// that fit on a page take, which are not cut. It ends at the lowest gap
/// in its lower half at least `gap` rows tall that is one color across,
/// so no line of text is cut in two; failing that, above a picture going
/// past its end; failing that, at a row like those either side of it, as
/// between the lines of a code block; failing that, after all `rows`.
pub fn page_end(
    pixels: &[u8],
    width: usize,
    rows: usize,
    gap: usize,
    pictures: &[Range<usize>],
) -> usize {
    let row = |index: usize| &pixels[index * width * 4..(index + 1) * width * 4];
    let inside = |index: usize| {
        pictures
            .iter()
            .any(|picture| picture.start < index && index < picture.end)
    };
    let earliest = (rows / 2).max(1);
    // Rows one color across, counted up from the lowest of them.
    let mut plain_above = 0;
    for index in (earliest..rows).rev() {
        if inside(index) || !plain(row(index)) {
            plain_above = 0;
            continue;
        }
        plain_above += 1;
        if plain_above >= gap.max(1) {
            return index + plain_above - 1;
        }
    }
    if let Some(top) = pictures
        .iter()
        .filter(|picture| picture.start > 0 && picture.end > rows)
        .map(|picture| picture.start)
        .min()
    {
        return top;
    }
    (earliest..rows.saturating_sub(1))
        .rev()
        .find(|&index| {
            !inside(index) && row(index) == row(index - 1) && row(index) == row(index + 1)
        })
        .unwrap_or(rows)
}

/// How many rows, up to `limit`, `pixels` starts with that are one color
/// across, for a page to start at what follows them.
pub fn plain_rows(pixels: &[u8], width: usize, limit: usize) -> usize {
    pixels
        .chunks_exact(width * 4)
        .take(limit)
        .take_while(|row| plain(row))
        .count()
}

fn plain(row: &[u8]) -> bool {
    let (pixels, _) = row.as_chunks::<4>();
    pixels.iter().all(|pixel| pixel == &pixels[0])
}

/// A printout under way: pages go to a thread that adds them to its PDF
/// as they come, so they need not all be held at once.
pub struct Printout {
    pages: mpsc::Sender<Page>,
    written: iced::futures::channel::oneshot::Receiver<Result<tempfile::NamedTempFile, String>>,
}

impl Default for Printout {
    fn default() -> Self {
        Self::new()
    }
}

impl Printout {
    pub fn new() -> Self {
        let (pages, received) = mpsc::channel::<Page>();
        let written = spawn(move || {
            let failed = |error: String| crate::fl!("print-failed", error = error);
            let mut document = PictureDocument::new();
            for page in received {
                document
                    .add(&page.picture, page.size, page.place)
                    .map_err(|error| failed(error.describe()))?;
            }
            let bytes = document
                .finish()
                .map_err(|error| failed(error.describe()))?;
            let file = tempfile::Builder::new()
                .prefix("prev-print-")
                .suffix(".pdf")
                .tempfile()
                .map_err(|error| failed(error.to_string()))?;
            std::fs::write(file.path(), bytes).map_err(|error| failed(error.to_string()))?;
            Ok(file)
        });
        Self { pages, written }
    }

    pub fn add(&self, page: Page) {
        // The thread only stops on a failure, which `print` reports.
        let _ = self.pages.send(page);
    }

    /// Shows the print dialog for the pages added, named `title` in the
    /// print queue. The file is gone once the dialog is done with it.
    pub fn print(self, title: String) -> impl Future<Output = Result<(), String>> + Send + 'static {
        let Self { pages, written } = self;
        drop(pages);
        async move {
            let file = written
                .await
                .unwrap_or_else(|_| Err(crate::fl!("print-stopped")))?;
            let result = crate::portal::print(file.path().to_owned(), title).await;
            drop(file);
            result
        }
    }
}

/// Prints `page` alone, named `title` in the print queue.
pub fn print_page(
    page: Page,
    title: String,
) -> impl Future<Output = Result<(), String>> + Send + 'static {
    let printout = Printout::new();
    printout.add(page);
    printout.print(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RGBA rows, each one gray, or for `x` a row of text: black and
    /// white in turn, the other way round from the row above.
    fn rows(colors: &[u8], width: usize) -> Vec<u8> {
        colors
            .iter()
            .enumerate()
            .flat_map(|(index, &color)| {
                (0..width).flat_map(move |column| {
                    let value = if color == b'x' {
                        ((column + index) % 2) as u8 * 255
                    } else {
                        color
                    };
                    [value, value, value, 255]
                })
            })
            .collect()
    }

    #[test]
    fn locales_name_their_region() {
        assert_eq!(posix_region("en_US.UTF-8").as_deref(), Some("US"));
        assert_eq!(posix_region("de_DE@euro").as_deref(), Some("DE"));
        assert_eq!(posix_region("C"), None);
    }

    #[test]
    fn pages_end_between_lines() {
        // Text, a gap, text: the page ends at the gap, not through the
        // last line, though there is room for some of it.
        let pixels = rows(b"xxxxxxxx\xffxxx", 3);
        assert_eq!(page_end(&pixels, 3, 12, 1, &[]), 8);
    }

    #[test]
    fn pages_end_at_gaps_not_at_lines_drawn_across() {
        // A gap two rows tall, then a picture with a line across it: the
        // page ends below the gap, leaving the picture whole.
        let pixels = rows(b"xxxxxx\xff\xffxx\x80xx", 3);
        assert_eq!(page_end(&pixels, 3, 13, 2, &[]), 7);
    }

    #[test]
    fn pictures_that_fit_on_a_page_go_whole_to_the_next() {
        // Text, a gap, and a picture from row 4 past the page's end with
        // a band of one color across it: the page ends above the picture.
        let pixels = rows(b"xxx\xffxxxxx\x80\x80x", 3);
        let picture = 4..14;
        assert_eq!(
            page_end(&pixels, 3, 12, 2, std::slice::from_ref(&picture)),
            4
        );
        // A picture taller than a page is not among them, and is cut.
        assert_eq!(page_end(&pixels, 3, 12, 2, &[]), 10);
    }

    #[test]
    fn pages_end_where_rows_repeat_without_a_gap() {
        // A code block's lines: rows alike either side of the cut.
        let mut colors = b"xxxxxxxx".to_vec();
        colors.extend([9, 9, 9]);
        colors.push(b'x');
        let mut pixels = rows(&colors, 3);
        // The block's background holds a mark in its first column.
        for row in 8..11 {
            pixels[row * 12] = 0;
        }
        assert_eq!(page_end(&pixels, 3, 12, 1, &[]), 9);
    }

    #[test]
    fn pages_without_a_gap_end_at_their_last_row() {
        let pixels = rows(&[b'x'; 9], 3);
        assert_eq!(page_end(&pixels, 3, 9, 1, &[]), 9);
    }

    #[test]
    fn pages_start_past_blank_rows() {
        let pixels = rows(b"\xff\xffxx\xff", 3);
        assert_eq!(plain_rows(&pixels, 3, 4), 2);
        assert_eq!(plain_rows(&pixels, 3, 1), 1);
    }

    #[test]
    fn pictures_print_at_their_size_up_to_the_longest_page() {
        let frame = |width, height| Frame {
            width,
            height,
            pixels: vec![0; (width * height * 4) as usize],
            delay: std::time::Duration::ZERO,
        };
        let page = picture_page(frame(400, 200), 400.0, 200.0);
        assert_eq!(page.size, Size::new(300.0, 150.0));
        assert_eq!(page.place, Rect::new(0.0, 0.0, 300.0, 150.0));
        let page = picture_page(frame(2, 1), 40000.0, 20000.0);
        assert_eq!(page.size, Size::new(14400.0, 7200.0));
    }
}
