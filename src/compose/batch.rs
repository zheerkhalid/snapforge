//! Batch print (GDI): many images, one print dialog, headers and footers, page splitting
//! at blank rows.

use super::split::split_image_rows;
use crate::common::time::DateTime;
use crate::common::win::{hwnd_from_isize, pcwstr, wide};
use crate::common::{Image, Rect};
use crate::effects::text::FontSpec;
use crate::settings::print::{MarginsMm, PrintFit, PrintSettings};
use anyhow::{bail, ensure, Result};
use std::path::{Path, PathBuf};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GlobalFree, COLORREF, HGLOBAL, RECT};
use windows::Win32::Graphics::Gdi::{
    DeleteDC, DeleteObject, DrawTextW, GetDeviceCaps, SelectObject, SetBkMode, SetBrushOrgEx, SetStretchBltMode,
    SetTextColor, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX,
    DT_SINGLELINE, DT_VCENTER, HALFTONE, HDC, HORZRES, LOGPIXELSX, LOGPIXELSY, PHYSICALHEIGHT, PHYSICALOFFSETX,
    PHYSICALOFFSETY, PHYSICALWIDTH, SRCCOPY, TRANSPARENT, VERTRES,
};
use windows::Win32::Storage::Xps::{AbortDoc, EndDoc, EndPage, StartDocW, StartPage, DOCINFOW};
use windows::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, PrintDlgW, PD_NOPAGENUMS, PD_NOSELECTION, PD_RETURNDC, PRINTDLGW,
};

// ------------------------------------------------------------------ pure layout

/// Printer geometry in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceGeom {
    /// Printable area size (HORZRES, VERTRES).
    pub printable_w: i32,
    pub printable_h: i32,
    /// Paper size (PHYSICALWIDTH, PHYSICALHEIGHT).
    pub paper_w: i32,
    pub paper_h: i32,
    /// Printable area offset from the paper edge (PHYSICALOFFSETX/Y).
    pub offset_x: i32,
    pub offset_y: i32,
    pub dpi_x: i32,
    pub dpi_y: i32,
}

/// Areas on a page, in device coordinates (origin = printable area's top-left).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageAreas {
    pub header: Rect,
    pub content: Rect,
    pub footer: Rect,
}

/// Content, header and footer areas for the margins. Header and footer bands of `text_h`
/// (plus a gap) are reserved inside the margins' box when present.
pub fn page_areas(g: &DeviceGeom, m: &MarginsMm, header: bool, footer: bool, text_h: i32) -> PageAreas {
    let mm = |v: f32, dpi: i32| (v.clamp(0.0, 200.0) / 25.4 * dpi as f32).round() as i32;
    let left = (mm(m.left, g.dpi_x) - g.offset_x).max(0);
    let top = (mm(m.top, g.dpi_y) - g.offset_y).max(0);
    let right = (g.paper_w - mm(m.right, g.dpi_x) - g.offset_x).min(g.printable_w).max(left + 1);
    let bottom = (g.paper_h - mm(m.bottom, g.dpi_y) - g.offset_y).min(g.printable_h).max(top + 1);
    let gap = text_h / 2;
    let band = text_h + gap;
    let mut content = Rect::from_ltrb(left, top, right, bottom);
    let mut hdr = Rect::new(left, top, right - left, 0);
    let mut ftr = Rect::new(left, bottom, right - left, 0);
    // Only reserve bands if at least half the page remains for the image.
    if header && content.h - band > content.h / 2 {
        hdr = Rect::new(left, top, right - left, text_h);
        content = Rect::from_ltrb(left, top + band, right, content.bottom());
    }
    if footer && content.h - band > content.h / 2 {
        ftr = Rect::new(left, bottom - text_h, right - left, text_h);
        content = Rect::from_ltrb(left, content.y, right, bottom - band);
    }
    PageAreas { header: hdr, content, footer: ftr }
}

/// One printed page of an image: source rows and destination rectangle (device pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrintSlice {
    pub y0: u32,
    pub y1: u32,
    pub dest: Rect,
}

/// Pages for one `w`x`h` image in `area`. `split(max_rows)` returns chunk end rows.
pub fn plan_print(
    w: u32,
    h: u32,
    area: Rect,
    fit: PrintFit,
    dpi_x: i32,
    center: bool,
    split: impl Fn(u32) -> Vec<u32>,
) -> Vec<PrintSlice> {
    if w == 0 || h == 0 || area.is_empty() {
        return Vec::new();
    }
    let (wf, hf) = (w as f64, h as f64);
    let (aw, ah) = (area.w as f64, area.h as f64);
    let place_x = |dw: i32| if center { area.x + (area.w - dw) / 2 } else { area.x };
    if fit == PrintFit::FitPage {
        let s = (aw / wf).min(ah / hf);
        let dw = ((wf * s).round() as i32).clamp(1, area.w);
        let dh = ((hf * s).round() as i32).clamp(1, area.h);
        let y = if center { area.y + (area.h - dh) / 2 } else { area.y };
        return vec![PrintSlice { y0: 0, y1: h, dest: Rect::new(place_x(dw), y, dw, dh) }];
    }
    let s = match fit {
        PrintFit::FitWidth => aw / wf,
        _ => (dpi_x.max(1) as f64 / 96.0).min(aw / wf),
    };
    let max_rows = ((ah / s).floor() as u32).max(1);
    let ends = if h <= max_rows { vec![h] } else { split(max_rows) };
    let mut out = Vec::new();
    let mut y0 = 0;
    for y1 in ends {
        let y1 = y1.clamp(y0 + 1, h);
        let dw = ((wf * s).round() as i32).clamp(1, area.w);
        let dh = (((y1 - y0) as f64 * s).round() as i32).clamp(1, area.h);
        out.push(PrintSlice { y0, y1, dest: Rect::new(place_x(dw), area.y, dw, dh) });
        y0 = y1;
        if y0 >= h {
            break;
        }
    }
    out
}

/// Values for header/footer templates.
#[derive(Debug, Clone, Default)]
pub struct TemplateCtx {
    pub file: String,
    pub path: String,
    pub page: usize,
    pub total: usize,
    pub date: String,
    pub time: String,
}

/// Replace `{file}`, `{path}`, `{n}` (or `{page}`), `{total}`, `{date}` and `{time}`.
pub fn expand_template(t: &str, c: &TemplateCtx) -> String {
    t.replace("{file}", &c.file)
        .replace("{path}", &c.path)
        .replace("{n}", &c.page.to_string())
        .replace("{page}", &c.page.to_string())
        .replace("{total}", &c.total.to_string())
        .replace("{date}", &c.date)
        .replace("{time}", &c.time)
}

// ------------------------------------------------------------------ printing

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

/// Show the print dialog once and print every image. Ok(false) if the user cancelled.
/// Images that fail to load are skipped (logged); it fails only if none can be printed.
pub fn batch_print(paths: &[PathBuf], opts: &PrintSettings, owner_hwnd: isize) -> Result<bool> {
    ensure!(!paths.is_empty(), "no images to print");
    let mut pd = PRINTDLGW {
        lStructSize: std::mem::size_of::<PRINTDLGW>() as u32,
        hwndOwner: hwnd_from_isize(owner_hwnd),
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
            log::info!("batch print: cancelled");
            return Ok(false);
        }
        bail!("the print dialog failed (error {:#x})", err.0);
    }
    ensure!(!res.hdc.is_invalid(), "the print dialog returned no printer");
    print_all(res.hdc, paths, opts)?;
    Ok(true)
}

fn device_geom(hdc: HDC) -> DeviceGeom {
    // SAFETY: plain queries on a valid DC.
    unsafe {
        DeviceGeom {
            printable_w: GetDeviceCaps(hdc, HORZRES),
            printable_h: GetDeviceCaps(hdc, VERTRES),
            paper_w: GetDeviceCaps(hdc, PHYSICALWIDTH),
            paper_h: GetDeviceCaps(hdc, PHYSICALHEIGHT),
            offset_x: GetDeviceCaps(hdc, PHYSICALOFFSETX),
            offset_y: GetDeviceCaps(hdc, PHYSICALOFFSETY),
            dpi_x: GetDeviceCaps(hdc, LOGPIXELSX).max(1),
            dpi_y: GetDeviceCaps(hdc, LOGPIXELSY).max(1),
        }
    }
}

fn print_all(hdc: HDC, paths: &[PathBuf], opts: &PrintSettings) -> Result<()> {
    let mut g = device_geom(hdc);
    if g.paper_w <= 0 || g.paper_h <= 0 {
        // Some drivers don't report physical sizes: treat the printable area as the paper.
        g = DeviceGeom { paper_w: g.printable_w, paper_h: g.printable_h, offset_x: 0, offset_y: 0, ..g };
    }
    let text_h = (opts.header_footer_pt.clamp(5.0, 36.0) / 72.0 * g.dpi_y as f32).round() as i32;
    let areas = page_areas(&g, &opts.margins, !opts.header.trim().is_empty(), !opts.footer.trim().is_empty(), text_h);
    let split = opts.avoid_splitting_lines;

    // Pass 1: plan every image so page totals are known.
    let mut jobs: Vec<(&PathBuf, Vec<PrintSlice>)> = Vec::new();
    for p in paths {
        match crate::output::save::load_image(p) {
            Ok(img) => {
                let plan = plan_print(img.width, img.height, areas.content, opts.fit, g.dpi_x, opts.center, |max| {
                    split_image_rows(&img, max, split)
                });
                jobs.push((p, plan));
            }
            Err(e) => log::warn!("batch print: skipping {}: {e:#}", p.display()),
        }
    }
    let total: usize = jobs.iter().map(|j| j.1.len()).sum();
    ensure!(total > 0, "none of the images could be loaded for printing");

    let now = DateTime::now_local();
    let date = format!("{:04}-{:02}-{:02}", now.year, now.month, now.day);
    let time = format!("{:02}:{:02}", now.hour, now.minute);
    let doc_name = wide(&format!("SnapForge ({} images)", jobs.len()));
    let di = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: pcwstr(&doc_name),
        lpszOutput: PCWSTR::null(),
        lpszDatatype: PCWSTR::null(),
        fwType: 0,
    };
    let font = crate::effects::text::create_font(&FontSpec::default(), text_h);
    // SAFETY: hdc is a valid printer DC; the font is selected out and deleted at the end.
    unsafe {
        if StartDocW(hdc, &di) <= 0 {
            let _ = DeleteObject(font);
            bail!("the printer refused the document (StartDoc failed)");
        }
        let old_font = SelectObject(hdc, font);
        let result = (|| -> Result<()> {
            let mut page = 0;
            for (path, plan) in &jobs {
                // Pass 2: load again (keeps only one image in memory at a time).
                let img = match crate::output::save::load_image(path) {
                    Ok(i) => i,
                    Err(e) => bail!("could not reload {}: {e:#}", path.display()),
                };
                let ctx_file = super::combine::file_label(path);
                for slice in plan {
                    page += 1;
                    if StartPage(hdc) <= 0 {
                        bail!("the printer refused page {page} (StartPage failed)");
                    }
                    let ctx = TemplateCtx {
                        file: ctx_file.clone(),
                        path: path.display().to_string(),
                        page,
                        total,
                        date: date.clone(),
                        time: time.clone(),
                    };
                    draw_text_line(hdc, &expand_template(&opts.header, &ctx), areas.header);
                    draw_text_line(hdc, &expand_template(&opts.footer, &ctx), areas.footer);
                    draw_slice(hdc, &img, slice)?;
                    if EndPage(hdc) <= 0 {
                        bail!("the printer failed to finish page {page} (EndPage)");
                    }
                }
            }
            Ok(())
        })();
        SelectObject(hdc, old_font);
        let _ = DeleteObject(font);
        match result {
            Ok(()) => {
                if EndDoc(hdc) <= 0 {
                    bail!("the printer failed to finish the document (EndDoc)");
                }
                log::info!("batch print: {} images, {total} pages", jobs.len());
                Ok(())
            }
            Err(e) => {
                let _ = AbortDoc(hdc);
                Err(e)
            }
        }
    }
}

fn draw_text_line(hdc: HDC, text: &str, r: Rect) {
    if text.trim().is_empty() || r.h <= 0 {
        return;
    }
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    let mut rc = RECT { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() };
    // SAFETY: valid DC; buffer and rect live across the call.
    unsafe {
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0x0040_4040));
        DrawTextW(hdc, &mut buf, &mut rc, DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS);
    }
}

fn draw_slice(hdc: HDC, img: &Image, s: &PrintSlice) -> Result<()> {
    let part = img.crop(Rect::new(0, s.y0 as i32, img.width as i32, (s.y1 - s.y0) as i32));
    let rgb = part.to_rgb_over_white();
    let mut bgrx = Vec::with_capacity(part.len_px() * 4);
    for p in rgb.chunks_exact(3) {
        bgrx.extend_from_slice(&[p[2], p[1], p[0], 0]);
    }
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: part.width as i32,
            biHeight: -(part.height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: valid DC; `bgrx` and `bmi` outlive the call.
    let lines = unsafe {
        SetStretchBltMode(hdc, HALFTONE);
        let _ = SetBrushOrgEx(hdc, 0, 0, None);
        StretchDIBits(
            hdc,
            s.dest.x,
            s.dest.y,
            s.dest.w,
            s.dest.h,
            0,
            0,
            part.width as i32,
            part.height as i32,
            Some(bgrx.as_ptr() as *const _),
            &bmi,
            DIB_RGB_COLORS,
            SRCCOPY,
        )
    };
    ensure!(lines != 0, "drawing the image on the printer failed (StretchDIBits)");
    Ok(())
}

/// File name for templates (exposed for the UI's preview).
pub fn template_preview(t: &str, path: &Path, page: usize, total: usize) -> String {
    let now = DateTime::now_local();
    expand_template(
        t,
        &TemplateCtx {
            file: super::combine::file_label(path),
            path: path.display().to_string(),
            page,
            total,
            date: format!("{:04}-{:02}-{:02}", now.year, now.month, now.day),
            time: format!("{:02}:{:02}", now.hour, now.minute),
        },
    )
}
