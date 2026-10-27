//! Printing on Windows: the system print dialog, then each chosen page
//! rendered by MuPDF at the printer's resolution and drawn through GDI,
//! scaled to fit the printable area. The driver makes the copies.

#![allow(unsafe_code)]

use std::path::Path;

use crate::i18n::Describe;
use prev_pdf::engine::{Bitmap, Document, Engine};
use prev_pdf::geometry::PixelRect;
use prev_pdf::mupdf_engine::MupdfEngine;
use windows::Win32::Foundation::{GlobalFree, HWND};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDCW, DIB_RGB_COLORS, DeleteDC, GetDeviceCaps,
    HALFTONE, HDC, HORZRES, LOGPIXELSX, LOGPIXELSY, SRCCOPY, SetStretchBltMode, StretchDIBits,
    VERTRES,
};
use windows::Win32::Graphics::Printing::{
    EnumPrintersW, GetDefaultPrinterW, PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL,
    PRINTER_INFO_4W,
};
use windows::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::UI::Controls::Dialogs::{
    PD_HIDEPRINTTOFILE, PD_NOCURRENTPAGE, PD_NOSELECTION, PD_PAGENUMS, PD_RESULT_PRINT,
    PD_RETURNDC, PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGEXW, PRINTPAGERANGE, PrintDlgExW,
    START_PAGE_GENERAL,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

/// The most device pixels per inch pages are rendered at; enough for
/// sharp print while keeping a page's pixels to a few tens of megabytes.
const MAX_DPI: i32 = 300;
const MAX_RANGES: usize = 16;

/// Prints the PDF at `path`, named `title` in the print queue. Blocks
/// until printing is handed to the spooler or cancelled.
pub fn print(path: &Path, title: &str) -> Result<(), String> {
    let document = MupdfEngine
        .open(path)
        .map_err(|error| crate::fl!("print-failed", error = error.describe()))?;
    let count = document
        .page_count()
        .map_err(|error| crate::fl!("print-failed", error = error.describe()))?;
    if count == 0 {
        return Ok(());
    }
    let Some((dc, pages)) = ask(count)? else {
        return Ok(());
    };
    let result = print_pages(dc, title, &pages, |page, dpi| render(&*document, page, dpi));
    // SAFETY: the dialog made this DC for us; it is released once.
    unsafe {
        let _ = DeleteDC(dc);
    }
    result.map_err(|error| crate::fl!("print-failed", error = error.to_string()))
}

/// Prints every page of the PDF at `path` on `printer`, or the default
/// one, without the dialog, named `title` in the print queue. Gives the
/// printer's name; why not, for agents, in English.
pub fn print_to(path: &Path, title: &str, printer: Option<&str>) -> Result<String, String> {
    let known = printers();
    let name = match printer {
        Some(wanted) => known
            .names
            .iter()
            .find(|name| name.eq_ignore_ascii_case(wanted))
            .cloned()
            .ok_or_else(|| {
                format!(
                    "There is no printer {wanted}; there are: {}.",
                    known.names.join(", ")
                )
            })?,
        None => known
            .default
            .ok_or("There is no default printer; name one from list_printers.")?,
    };
    let document = MupdfEngine
        .open(path)
        .map_err(|error| crate::fl!("print-failed", error = error.describe()))?;
    let count = document
        .page_count()
        .map_err(|error| crate::fl!("print-failed", error = error.describe()))?;
    // SAFETY: a DC for the printer by name, with its own settings; it is
    // released below.
    let dc = unsafe {
        CreateDCW(
            w!("WINSPOOL"),
            &HSTRING::from(name.as_str()),
            PCWSTR::null(),
            None,
        )
    };
    if dc.is_invalid() {
        return Err(format!("Could not open the printer {name}."));
    }
    let pages: Vec<usize> = (0..count).collect();
    let result = print_pages(dc, title, &pages, |page, dpi| render(&*document, page, dpi));
    // SAFETY: the DC made above, released once.
    unsafe {
        let _ = DeleteDC(dc);
    }
    result
        .map(|()| name)
        .map_err(|error| crate::fl!("print-failed", error = error.to_string()))
}

/// The printers Windows knows of, on this computer or connected, and its
/// default one.
pub fn printers() -> crate::portal::Printers {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let (mut needed, mut count) = (0u32, 0u32);
    // SAFETY: with no buffer, the call only says how much room the list
    // needs.
    let _ = unsafe { EnumPrintersW(flags, PCWSTR::null(), 4, None, &mut needed, &mut count) };
    let mut names = Vec::new();
    if needed > 0 {
        // Words, so the list's pointers sit aligned.
        let mut words = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: the bytes of `words`, which outlive their use here.
        let buffer = unsafe {
            std::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), needed as usize)
        };
        // SAFETY: the buffer is as large as the first call asked for.
        if unsafe {
            EnumPrintersW(
                flags,
                PCWSTR::null(),
                4,
                Some(buffer),
                &mut needed,
                &mut count,
            )
        }
        .is_ok()
        {
            // SAFETY: the buffer now holds `count` entries, whose names
            // point into it.
            let entries = unsafe {
                std::slice::from_raw_parts(words.as_ptr().cast::<PRINTER_INFO_4W>(), count as usize)
            };
            for entry in entries {
                // SAFETY: each name is a terminated string in the buffer.
                if let Ok(name) = unsafe { entry.pPrinterName.to_string() } {
                    names.push(name);
                }
            }
        }
    }
    let mut length = 0u32;
    // SAFETY: with no buffer, the call only says how long the name is.
    let _ = unsafe { GetDefaultPrinterW(None, &mut length) };
    let mut chars = vec![0u16; length as usize];
    // SAFETY: the buffer holds `length` characters, the name's ending
    // included.
    let default = (length > 0
        && unsafe { GetDefaultPrinterW(Some(PWSTR(chars.as_mut_ptr())), &mut length) }.as_bool())
    .then(|| String::from_utf16_lossy(&chars[..length.saturating_sub(1) as usize]));
    crate::portal::Printers {
        names,
        default,
        dialog_only: false,
    }
}

/// Page `page` of `document` at `dpi` device pixels per inch.
fn render(document: &dyn Document, page: usize, dpi: f32) -> Result<Bitmap, String> {
    let display = document.display(page).map_err(|error| error.describe())?;
    let size = display.size();
    let scale = dpi / 72.0;
    display
        .render(
            scale,
            PixelRect {
                x: 0,
                y: 0,
                width: (size.width * scale).ceil() as u32,
                height: (size.height * scale).ceil() as u32,
            },
        )
        .map_err(|error| error.describe())
}

/// Shows the print dialog. The printer's DC and the pages to print, from
/// zero, or `None` when cancelled.
fn ask(count: usize) -> Result<Option<(HDC, Vec<usize>)>, String> {
    let count = count as u32;
    let mut ranges = [PRINTPAGERANGE::default(); MAX_RANGES];
    ranges[0] = PRINTPAGERANGE {
        nFromPage: 1,
        nToPage: count,
    };
    let mut dialog = PRINTDLGEXW {
        lStructSize: std::mem::size_of::<PRINTDLGEXW>() as u32,
        // SAFETY: only reads which window is in front, to own the dialog.
        hwndOwner: unsafe { GetForegroundWindow() },
        Flags: PD_RETURNDC
            | PD_NOSELECTION
            | PD_NOCURRENTPAGE
            | PD_HIDEPRINTTOFILE
            | PD_USEDEVMODECOPIESANDCOLLATE,
        nPageRanges: 1,
        nMaxPageRanges: MAX_RANGES as u32,
        lpPageRanges: ranges.as_mut_ptr(),
        nMinPage: 1,
        nMaxPage: count,
        nCopies: 1,
        nStartPage: START_PAGE_GENERAL,
        ..Default::default()
    };
    if dialog.hwndOwner == HWND::default() {
        return Err(crate::fl!("print-no-window"));
    }
    // SAFETY: the dialog and its ranges live through the call; the handles
    // it returns are freed below.
    unsafe {
        let _ = OleInitialize(None);
        PrintDlgExW(&mut dialog)
            .map_err(|error| crate::fl!("print-dialog-failed", error = error.to_string()))?;
        let _ = GlobalFree(Some(dialog.hDevMode));
        let _ = GlobalFree(Some(dialog.hDevNames));
    }
    if dialog.dwResultAction != PD_RESULT_PRINT {
        if !dialog.hDC.is_invalid() {
            // SAFETY: the DC is ours and not used again.
            unsafe {
                let _ = DeleteDC(dialog.hDC);
            }
        }
        return Ok(None);
    }
    let chosen: Vec<(u32, u32)> = if dialog.Flags.0 & PD_PAGENUMS.0 != 0 {
        ranges[..dialog.nPageRanges as usize]
            .iter()
            .map(|range| (range.nFromPage, range.nToPage))
            .collect()
    } else {
        vec![(1, count)]
    };
    Ok(Some((dialog.hDC, pages_in(&chosen, count))))
}

/// The pages, from zero, in the dialog's 1-based ranges, in order.
fn pages_in(ranges: &[(u32, u32)], count: u32) -> Vec<usize> {
    ranges
        .iter()
        .flat_map(|&(from, to)| {
            let (from, to) = (from.min(to).max(1), from.max(to).min(count));
            from..=to
        })
        .map(|page| page as usize - 1)
        .collect()
}

/// Where a page `width` by `height` goes in a printable area, keeping its
/// shape: (x, y, width, height), centered.
fn fit(width: f32, height: f32, area_width: i32, area_height: i32) -> (i32, i32, i32, i32) {
    let scale = (area_width as f32 / width).min(area_height as f32 / height);
    let (fitted_width, fitted_height) = (
        (width * scale).round() as i32,
        (height * scale).round() as i32,
    );
    (
        (area_width - fitted_width) / 2,
        (area_height - fitted_height) / 2,
        fitted_width,
        fitted_height,
    )
}

fn print_pages(
    dc: HDC,
    title: &str,
    pages: &[usize],
    render: impl Fn(usize, f32) -> Result<Bitmap, String>,
) -> Result<(), String> {
    let name = HSTRING::from(title);
    let document = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: windows::core::PCWSTR(name.as_ptr()),
        ..Default::default()
    };
    // SAFETY: the DC is the printer's; the calls follow the GDI printing
    // sequence, and every bitmap lives through its StretchDIBits.
    unsafe {
        if StartDocW(dc, &document) <= 0 {
            return Err(crate::fl!("print-job-not-started"));
        }
        let area = (
            GetDeviceCaps(Some(dc), HORZRES),
            GetDeviceCaps(Some(dc), VERTRES),
        );
        let dpi = GetDeviceCaps(Some(dc), LOGPIXELSX)
            .min(GetDeviceCaps(Some(dc), LOGPIXELSY))
            .clamp(72, MAX_DPI) as f32;
        SetStretchBltMode(dc, HALFTONE);
        for &page in pages {
            let bitmap = match render(page, dpi) {
                Ok(bitmap) => bitmap,
                Err(error) => {
                    AbortDoc(dc);
                    return Err(error);
                }
            };
            if StartPage(dc) <= 0 {
                AbortDoc(dc);
                return Err(crate::fl!("print-printer-stopped"));
            }
            let (x, y, width, height) =
                fit(bitmap.width as f32, bitmap.height as f32, area.0, area.1);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: bitmap.width as i32,
                    // Negative: rows run from the top, as rendered.
                    biHeight: -(bitmap.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let pixels = bgra_on_white(&bitmap.pixels);
            StretchDIBits(
                dc,
                x,
                y,
                width,
                height,
                0,
                0,
                bitmap.width as i32,
                bitmap.height as i32,
                Some(pixels.as_ptr().cast()),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            if EndPage(dc) <= 0 {
                AbortDoc(dc);
                return Err(crate::fl!("print-printer-stopped"));
            }
        }
        EndDoc(dc);
    }
    Ok(())
}

/// RGBA pixels as the BGRA a DIB holds, over white where see-through.
fn bgra_on_white(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[red, green, blue, alpha]| {
            let over = |channel: u8| {
                let alpha = u16::from(alpha);
                ((u16::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8
            };
            [over(blue), over(green), over(red), 0]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_become_pages_in_order() {
        assert_eq!(pages_in(&[(1, 3)], 10), vec![0, 1, 2]);
        assert_eq!(pages_in(&[(5, 5), (2, 3)], 10), vec![4, 1, 2]);
        assert_eq!(pages_in(&[(8, 20)], 10), vec![7, 8, 9]);
    }

    #[test]
    fn pages_fit_the_printable_area_centered() {
        // A letter page on an A4-shaped area is limited by its width.
        let (x, y, width, height) = fit(612.0, 792.0, 4800, 7000);
        assert_eq!((x, width), (0, 4800));
        assert!(height <= 7000 && y * 2 + height == 7000);
    }

    #[test]
    fn transparent_pixels_print_white() {
        assert_eq!(
            bgra_on_white(&[10, 20, 30, 255, 0, 0, 0, 0]),
            vec![30, 20, 10, 0, 255, 255, 255, 0]
        );
    }
}
