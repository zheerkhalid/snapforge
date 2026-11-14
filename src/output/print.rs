//! Printing via the common Print dialog and GDI.

use super::dib;
use crate::common::win::{hwnd_from_isize, pcwstr, wide};
use crate::common::Image;
use anyhow::{bail, ensure, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows::Win32::Graphics::Gdi::{
    DeleteDC, GetDeviceCaps, SetBrushOrgEx, SetStretchBltMode, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER,
    DIB_RGB_COLORS, HALFTONE, HDC, HORZRES, SRCCOPY, VERTRES,
};
use windows::Win32::Storage::Xps::{AbortDoc, EndDoc, EndPage, StartDocW, StartPage, DOCINFOW};
use windows::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, PrintDlgW, PD_NOPAGENUMS, PD_NOSELECTION, PD_RETURNDC, PRINTDLGW,
};

/// Placement of an image inside the printable area: (x, y, width, height) in device pixels.
/// Scales to fit (up or down) keeping the aspect ratio, centred.
pub fn fit_rect(img_w: u32, img_h: u32, area_w: i32, area_h: i32) -> (i32, i32, i32, i32) {
    if img_w == 0 || img_h == 0 || area_w <= 0 || area_h <= 0 {
        return (0, 0, 0, 0);
    }
    let sx = area_w as f64 / img_w as f64;
    let sy = area_h as f64 / img_h as f64;
    let s = sx.min(sy);
    let w = ((img_w as f64 * s).round() as i32).clamp(1, area_w);
    let h = ((img_h as f64 * s).round() as i32).clamp(1, area_h);
    ((area_w - w) / 2, (area_h - h) / 2, w, h)
}

/// Frees the dialog's DEVMODE / DEVNAMES handles and the printer DC.
struct DialogResources {
    devmode: HGLOBAL,
    devnames: HGLOBAL,
    hdc: HDC,
}

impl Drop for DialogResources {
    fn drop(&mut self) {
        // SAFETY: handles came from PrintDlgW and are owned by us; null ones are skipped.
        unsafe {
            if !self.hdc.is_invalid() {
                let _ = DeleteDC(self.hdc);
            }
            if !self.devmode.is_invalid() {
                let _ = GlobalFree(self.devmode);
            }
            if !self.devnames.is_invalid() {
                let _ = GlobalFree(self.devnames);
            }
        }
    }
}

/// Show the print dialog (owner window `owner`, may be 0) and print the image fitted
/// to the page, centred, keeping the aspect ratio. Ok(false) if the user cancelled.
pub fn print_image(img: &Image, owner: isize, title: &str) -> Result<bool> {
    ensure!(!img.is_empty(), "nothing to print (empty image)");
    let mut pd = PRINTDLGW {
        lStructSize: std::mem::size_of::<PRINTDLGW>() as u32,
        hwndOwner: hwnd_from_isize(owner),
        Flags: PD_RETURNDC | PD_NOSELECTION | PD_NOPAGENUMS,
        nCopies: 1,
        ..Default::default()
    };
    // SAFETY: pd is a correctly sized PRINTDLGW.
    let ok = unsafe { PrintDlgW(&mut pd) }.as_bool();
    let res = DialogResources { devmode: pd.hDevMode, devnames: pd.hDevNames, hdc: pd.hDC };
    if !ok {
        // SAFETY: plain query.
        let err = unsafe { CommDlgExtendedError() };
        if err.0 == 0 {
            log::info!("print: cancelled");
            return Ok(false);
        }
        bail!("the print dialog failed (error {:#x})", err.0);
    }
    ensure!(!res.hdc.is_invalid(), "the print dialog returned no printer");
    print_to_dc(res.hdc, img, title)?;
    log::info!("print: sent {}x{} image \"{title}\"", img.width, img.height);
    Ok(true)
}

fn print_to_dc(hdc: HDC, img: &Image, title: &str) -> Result<()> {
    let doc_name = wide(if title.trim().is_empty() { "SnapForge" } else { title });
    let di = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: pcwstr(&doc_name),
        lpszOutput: PCWSTR::null(),
        lpszDatatype: PCWSTR::null(),
        fwType: 0,
    };
    let pixels = dib::bgrx_over_white(img);
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: img.width as i32,
            biHeight: -(img.height as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: hdc is a valid printer DC; `di`, `bmi` and `pixels` outlive the calls.
    unsafe {
        if StartDocW(hdc, &di) <= 0 {
            bail!("the printer refused the document (StartDoc failed)");
        }
        if StartPage(hdc) <= 0 {
            let _ = AbortDoc(hdc);
            bail!("the printer refused the page (StartPage failed)");
        }
        let area_w = GetDeviceCaps(hdc, HORZRES);
        let area_h = GetDeviceCaps(hdc, VERTRES);
        let (x, y, w, h) = fit_rect(img.width, img.height, area_w, area_h);
        SetStretchBltMode(hdc, HALFTONE);
        let _ = SetBrushOrgEx(hdc, 0, 0, None);
        let lines = StretchDIBits(
            hdc,
            x,
            y,
            w,
            h,
            0,
            0,
            img.width as i32,
            img.height as i32,
            Some(pixels.as_ptr() as *const _),
            &bmi,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
        if lines == 0 {
            let _ = AbortDoc(hdc);
            bail!("drawing the image on the printer failed (StretchDIBits)");
        }
        if EndPage(hdc) <= 0 {
            let _ = AbortDoc(hdc);
            bail!("the printer failed to finish the page (EndPage)");
        }
        if EndDoc(hdc) <= 0 {
            bail!("the printer failed to finish the document (EndDoc)");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_wide_image() {
        // 2000x1000 into 4800x6400 (portrait page): width-limited.
        assert_eq!(fit_rect(2000, 1000, 4800, 6400), (0, 2000, 4800, 2400));
    }

    #[test]
    fn fits_tall_image() {
        assert_eq!(fit_rect(100, 400, 1000, 1000), (375, 0, 250, 1000));
    }

    #[test]
    fn upscales_small_and_keeps_aspect() {
        let (x, y, w, h) = fit_rect(30, 20, 3000, 3000);
        assert_eq!((w, h), (3000, 2000));
        assert_eq!((x, y), (0, 500));
    }

    #[test]
    fn exact_fit_and_degenerate() {
        assert_eq!(fit_rect(500, 500, 500, 500), (0, 0, 500, 500));
        assert_eq!(fit_rect(0, 10, 500, 500), (0, 0, 0, 0));
        assert_eq!(fit_rect(10, 10, 0, 500), (0, 0, 0, 0));
        // Extreme aspect still gets at least one device pixel.
        let (_, _, w, h) = fit_rect(100_000, 1, 1000, 1000);
        assert_eq!((w, h), (1000, 1));
    }
}
