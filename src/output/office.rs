//! Send to Word / Excel / PowerPoint via COM late binding (IDispatch), and OneNote via the
//! clipboard (DECISIONS #6).

use super::{clipboard, save};
use crate::common::win::{pcwstr, wide};
use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;
use std::time::{Duration, Instant};
use windows::core::{IUnknown, Interface, BSTR, GUID, HRESULT, PCWSTR, VARIANT};
use windows::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{
    CLSIDFromProgID, CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch, CLSCTX_LOCAL_SERVER,
    COINIT_APARTMENTTHREADED, DISPATCH_FLAGS, DISPATCH_METHOD, DISPATCH_PROPERTYGET, DISPATCH_PROPERTYPUT, DISPPARAMS,
    EXCEPINFO,
};
use windows::Win32::System::Ole::{GetActiveObject, DISPID_PROPERTYPUT};
use windows::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, HKEY, HKEY_CLASSES_ROOT, KEY_READ};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, IsIconic, SetForegroundWindow, ShowWindow, ASFW_ANY, SW_RESTORE, SW_SHOWNORMAL,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficeApp {
    Word,
    Excel,
    PowerPoint,
    OneNote,
}

impl OfficeApp {
    pub fn prog_id(self) -> &'static str {
        match self {
            OfficeApp::Word => "Word.Application",
            OfficeApp::Excel => "Excel.Application",
            OfficeApp::PowerPoint => "PowerPoint.Application",
            OfficeApp::OneNote => "OneNote.Application",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            OfficeApp::Word => "Word",
            OfficeApp::Excel => "Excel",
            OfficeApp::PowerPoint => "PowerPoint",
            OfficeApp::OneNote => "OneNote",
        }
    }
}

// ---------------------------------------------------------------- COM helpers

const RPC_E_CALL_REJECTED: HRESULT = HRESULT(0x8001_0001_u32 as i32);
const RPC_E_SERVERCALL_RETRYLATER: HRESULT = HRESULT(0x8001_010A_u32 as i32);
const VT_DISPATCH: u16 = 9;
const VT_UNKNOWN: u16 = 13;
const LOCALE_USER_DEFAULT: u32 = 0x0400;

/// Balances a successful CoInitializeEx on this thread.
pub struct ComInit(bool);

impl ComInit {
    pub fn sta() -> Self {
        // SAFETY: plain call; S_FALSE (already initialised) still needs CoUninitialize.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            // Thread is already MTA; COM works, just don't uninitialise.
            ComInit(false)
        } else {
            ComInit(hr.is_ok())
        }
    }
}

impl Drop for ComInit {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }
}

/// A typed argument for [`Disp::call`] and friends.
pub enum Arg<'a> {
    Str(&'a str),
    I32(i32),
    Bool(bool),
    F64(f64),
    Disp(&'a IDispatch),
}

impl Arg<'_> {
    fn to_variant(&self) -> VARIANT {
        match self {
            Arg::Str(s) => VARIANT::from(BSTR::from(*s)),
            Arg::I32(v) => VARIANT::from(*v),
            Arg::Bool(v) => VARIANT::from(*v),
            Arg::F64(v) => VARIANT::from(*v),
            Arg::Disp(d) => dispatch_variant(d),
        }
    }
}

/// VT_DISPATCH variant holding a new reference to `d`.
fn dispatch_variant(d: &IDispatch) -> VARIANT {
    use windows::core::imp;
    let raw = imp::VARIANT {
        Anonymous: imp::VARIANT_0 {
            Anonymous: imp::VARIANT_0_0 {
                vt: VT_DISPATCH,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: imp::VARIANT_0_0_0 { pdispVal: d.clone().into_raw() },
            },
        },
    };
    // SAFETY: the variant owns the reference created by clone().into_raw(); VariantClear on drop releases it.
    unsafe { VARIANT::from_raw(raw) }
}

/// Late-bound IDispatch wrapper.
#[derive(Clone)]
pub struct Disp(pub IDispatch);

impl Disp {
    fn dispid(&self, name: &str) -> Result<i32> {
        let w = wide(name);
        let names = [pcwstr(&w)];
        let mut id = 0i32;
        // SAFETY: one name, one out id.
        unsafe { self.0.GetIDsOfNames(&GUID::zeroed(), names.as_ptr(), 1, LOCALE_USER_DEFAULT, &mut id) }
            .with_context(|| format!("COM member \"{name}\" not found"))?;
        Ok(id)
    }

    fn invoke(&self, name: &str, flags: DISPATCH_FLAGS, args: &[Arg]) -> Result<VARIANT> {
        let id = self.dispid(name)?;
        // IDispatch takes arguments in reverse order.
        let mut vargs: Vec<VARIANT> = args.iter().rev().map(Arg::to_variant).collect();
        let mut named = DISPID_PROPERTYPUT;
        let is_put = flags == DISPATCH_PROPERTYPUT;
        let params = DISPPARAMS {
            rgvarg: if vargs.is_empty() { std::ptr::null_mut() } else { vargs.as_mut_ptr() },
            rgdispidNamedArgs: if is_put { &mut named } else { std::ptr::null_mut() },
            cArgs: vargs.len() as u32,
            cNamedArgs: u32::from(is_put),
        };
        let start = Instant::now();
        loop {
            let mut result = VARIANT::new();
            let mut excep = EXCEPINFO::default();
            let mut arg_err = 0u32;
            // SAFETY: params/result/excep point to live locals for the duration of the call.
            let r = unsafe {
                self.0.Invoke(
                    id,
                    &GUID::zeroed(),
                    LOCALE_USER_DEFAULT,
                    flags,
                    &params,
                    Some(&mut result),
                    Some(&mut excep),
                    Some(&mut arg_err),
                )
            };
            match r {
                Ok(()) => return Ok(result),
                // The app is busy (e.g. still starting or a dialog is open): retry for a while.
                Err(e)
                    if (e.code() == RPC_E_CALL_REJECTED || e.code() == RPC_E_SERVERCALL_RETRYLATER)
                        && start.elapsed() < Duration::from_secs(15) =>
                {
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => {
                    let desc = excep.bstrDescription.to_string();
                    let detail = if desc.is_empty() { e.message() } else { desc };
                    bail!("COM call \"{name}\" failed: {detail} ({:#010x})", e.code().0);
                }
            }
        }
    }

    pub fn get(&self, name: &str) -> Result<VARIANT> {
        self.invoke(name, DISPATCH_PROPERTYGET, &[])
    }
    pub fn get_obj(&self, name: &str) -> Result<Disp> {
        to_disp(&self.get(name)?).with_context(|| format!("\"{name}\" is not an object"))
    }
    pub fn get_i32(&self, name: &str) -> Result<i32> {
        i32::try_from(&self.get(name)?).map_err(|e| anyhow!("\"{name}\" is not a number: {e}"))
    }
    pub fn get_f64(&self, name: &str) -> Result<f64> {
        f64::try_from(&self.get(name)?).map_err(|e| anyhow!("\"{name}\" is not a number: {e}"))
    }
    pub fn put(&self, name: &str, value: Arg) -> Result<()> {
        self.invoke(name, DISPATCH_PROPERTYPUT, &[value]).map(|_| ())
    }
    pub fn call(&self, name: &str, args: &[Arg]) -> Result<VARIANT> {
        self.invoke(name, DISPATCH_METHOD | DISPATCH_PROPERTYGET, args)
    }
    pub fn call_obj(&self, name: &str, args: &[Arg]) -> Result<Disp> {
        to_disp(&self.call(name, args)?).with_context(|| format!("\"{name}\" did not return an object"))
    }
}

/// Extract an IDispatch from a VT_DISPATCH / VT_UNKNOWN variant.
fn to_disp(v: &VARIANT) -> Result<Disp> {
    let raw = v.as_raw();
    // SAFETY: reading the tag and the pointer arm that matches it.
    unsafe {
        let vt = raw.Anonymous.Anonymous.vt;
        let p = raw.Anonymous.Anonymous.Anonymous.pdispVal;
        if (vt == VT_DISPATCH || vt == VT_UNKNOWN) && !p.is_null() {
            let unk = IUnknown::from_raw_borrowed(&p).ok_or_else(|| anyhow!("null object"))?;
            return Ok(Disp(unk.cast::<IDispatch>()?));
        }
        bail!("expected an object, got variant type {vt}")
    }
}

/// Running instance of `prog_id`, or a new one (out of process).
pub fn connect(prog_id: &str) -> Result<Disp> {
    let w = wide(prog_id);
    // SAFETY: valid NUL-terminated ProgID.
    let clsid = unsafe { CLSIDFromProgID(pcwstr(&w)) }.with_context(|| format!("{prog_id} is not installed"))?;
    let mut unk: Option<IUnknown> = None;
    // SAFETY: valid CLSID and out pointer.
    if unsafe { GetActiveObject(&clsid, None, &mut unk) }.is_ok() {
        if let Some(u) = unk {
            if let Ok(d) = u.cast::<IDispatch>() {
                log::debug!("office: attached to running {prog_id}");
                return Ok(Disp(d));
            }
        }
    }
    // SAFETY: valid CLSID; the result is a counted interface.
    let d: IDispatch = unsafe { CoCreateInstance(&clsid, None, CLSCTX_LOCAL_SERVER) }
        .with_context(|| format!("could not start {prog_id}"))?;
    log::debug!("office: started {prog_id}");
    Ok(Disp(d))
}

fn progid_registered(prog_id: &str) -> bool {
    let w = wide(prog_id);
    // SAFETY: valid NUL-terminated string.
    unsafe { CLSIDFromProgID(pcwstr(&w)) }.is_ok()
}

fn hkcr_key_exists(name: &str) -> bool {
    let w = wide(name);
    let mut key = HKEY::default();
    // SAFETY: valid strings and out pointer; the key is closed when opened.
    unsafe {
        if RegOpenKeyExW(HKEY_CLASSES_ROOT, pcwstr(&w), 0, KEY_READ, &mut key).is_ok() {
            let _ = RegCloseKey(key);
            true
        } else {
            false
        }
    }
}

/// Whether the app is installed (ProgID registered).
pub fn is_installed(app: OfficeApp) -> bool {
    match app {
        OfficeApp::OneNote => progid_registered(app.prog_id()) || hkcr_key_exists("onenote"),
        _ => progid_registered(app.prog_id()),
    }
}

fn bring_to_front(app: &Disp, which: OfficeApp) {
    // SAFETY: lets the Office process take the foreground when it activates itself.
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    let hwnd = match which {
        OfficeApp::Word => app.get_obj("ActiveWindow").and_then(|w| w.get_i32("Hwnd")),
        OfficeApp::Excel => app.get_i32("Hwnd"),
        OfficeApp::PowerPoint => app.get_i32("HWND"),
        OfficeApp::OneNote => return,
    };
    match hwnd {
        Ok(h) if h != 0 => {
            let hwnd = HWND(h as isize as *mut std::ffi::c_void);
            // SAFETY: plain window calls; harmless if the handle is stale.
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
            }
        }
        _ => {
            if which == OfficeApp::Word {
                let _ = app.call("Activate", &[]);
            }
        }
    }
}

fn send_word(app: &Disp, path: &str) -> Result<()> {
    app.put("Visible", Arg::Bool(true))?;
    let docs = app.get_obj("Documents")?;
    if docs.get_i32("Count")? == 0 {
        docs.call("Add", &[])?;
    }
    let sel = app.get_obj("Selection")?;
    let shapes = sel.get_obj("InlineShapes")?;
    shapes.call("AddPicture", &[Arg::Str(path)])?;
    Ok(())
}

fn send_excel(app: &Disp, path: &str) -> Result<()> {
    app.put("Visible", Arg::Bool(true))?;
    let books = app.get_obj("Workbooks")?;
    if books.get_i32("Count")? == 0 {
        books.call("Add", &[])?;
    }
    let sheet = app.get_obj("ActiveSheet")?;
    let (left, top) = match app.get_obj("ActiveCell") {
        Ok(cell) => (cell.get_f64("Left").unwrap_or(0.0), cell.get_f64("Top").unwrap_or(0.0)),
        Err(_) => (0.0, 0.0),
    };
    let shapes = sheet.get_obj("Shapes")?;
    shapes.call(
        "AddPicture",
        &[Arg::Str(path), Arg::I32(0), Arg::I32(-1), Arg::F64(left), Arg::F64(top), Arg::F64(-1.0), Arg::F64(-1.0)],
    )?;
    Ok(())
}

fn send_powerpoint(app: &Disp, path: &str) -> Result<()> {
    // MsoTriState: msoTrue = -1.
    app.put("Visible", Arg::I32(-1))?;
    let pres_list = app.get_obj("Presentations")?;
    if pres_list.get_i32("Count")? == 0 {
        pres_list.call("Add", &[])?;
    }
    let pres = app.get_obj("ActivePresentation")?;
    let slides = pres.get_obj("Slides")?;
    let index = slides.get_i32("Count")? + 1;
    const PP_LAYOUT_BLANK: i32 = 12;
    let slide = slides.call_obj("Add", &[Arg::I32(index), Arg::I32(PP_LAYOUT_BLANK)])?;
    let shapes = slide.get_obj("Shapes")?;
    let pic =
        shapes.call_obj("AddPicture", &[Arg::Str(path), Arg::I32(0), Arg::I32(-1), Arg::F64(0.0), Arg::F64(0.0)])?;

    let setup = pres.get_obj("PageSetup")?;
    let (sw, sh) = (setup.get_f64("SlideWidth")?, setup.get_f64("SlideHeight")?);
    let (mut pw, mut ph) = (pic.get_f64("Width")?, pic.get_f64("Height")?);
    if pw > sw || ph > sh {
        // Shrink to fit (aspect ratio is locked by default, so setting Width scales Height).
        let s = (sw / pw).min(sh / ph);
        pic.put("LockAspectRatio", Arg::I32(-1))?;
        pic.put("Width", Arg::F64(pw * s))?;
        pw = pic.get_f64("Width")?;
        ph = pic.get_f64("Height")?;
    }
    pic.put("Left", Arg::F64((sw - pw) / 2.0))?;
    pic.put("Top", Arg::F64((sh - ph) / 2.0))?;
    if let Ok(win) = app.get_obj("ActiveWindow") {
        if let Ok(view) = win.get_obj("View") {
            let _ = view.call("GotoSlide", &[Arg::I32(index)]);
        }
    }
    Ok(())
}

fn shell_execute(file: &str) -> bool {
    let f = wide(file);
    let verb = wide("open");
    // SAFETY: valid NUL-terminated strings.
    let r = unsafe {
        ShellExecuteW(HWND::default(), pcwstr(&verb), pcwstr(&f), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
    };
    r.0 as isize > 32
}

fn send_onenote(path: &Path) -> Result<()> {
    let img = save::load_image(path)?;
    clipboard::copy_image(&img).context("could not copy the image for OneNote")?;
    // SAFETY: lets OneNote take the foreground.
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    // App Paths resolves onenote.exe (desktop OneNote); the protocol covers the Store app.
    if !shell_execute("onenote.exe") && !shell_execute("onenote:") {
        bail!("OneNote could not be started; the image is on the clipboard, paste it into OneNote manually");
    }
    log::info!("office: image copied and OneNote activated; the user should paste it (Ctrl+V)");
    Ok(())
}

/// Insert the image file into the running (or a newly started) Office app via COM.
pub fn send_to(app: OfficeApp, path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("cannot send {} to {}: the file does not exist", path.display(), app.name());
    }
    if app == OfficeApp::OneNote {
        return send_onenote(path);
    }
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_string_lossy();
    let path_str = s
        .strip_prefix(r"\\?\UNC\")
        .map(|r| format!(r"\\{r}"))
        .unwrap_or_else(|| s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or_else(|| s.to_string()));

    let _com = ComInit::sta();
    let disp = connect(app.prog_id()).with_context(|| format!("{} is not available", app.name()))?;
    let r = match app {
        OfficeApp::Word => send_word(&disp, &path_str),
        OfficeApp::Excel => send_excel(&disp, &path_str),
        OfficeApp::PowerPoint => send_powerpoint(&disp, &path_str),
        OfficeApp::OneNote => send_onenote(path),
    };
    r.with_context(|| format!("could not insert the image into {}", app.name()))?;
    bring_to_front(&disp, app);
    log::info!("office: inserted {} into {}", path_str, app.name());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_and_installed_checks_do_not_panic() {
        let _com = ComInit::sta();
        for app in [OfficeApp::Word, OfficeApp::Excel, OfficeApp::PowerPoint, OfficeApp::OneNote] {
            let _ = is_installed(app);
        }
        assert!(!progid_registered("SnapForge.Definitely.NotRegistered"));
        assert!(connect("SnapForge.Definitely.NotRegistered").is_err());
        assert!(send_to(OfficeApp::Word, Path::new(r"C:\missing\nothing.png")).is_err());
    }

    #[test]
    fn late_binding_against_a_system_object() {
        // Shell.Application is an in-box automation object; exercise GetIDsOfNames/Invoke on it.
        let _com = ComInit::sta();
        let w = wide("Shell.Application");
        // SAFETY: valid ProgID string.
        let Ok(clsid) = (unsafe { CLSIDFromProgID(pcwstr(&w)) }) else { return };
        // SAFETY: valid CLSID.
        let d: IDispatch =
            match unsafe { CoCreateInstance(&clsid, None, windows::Win32::System::Com::CLSCTX_INPROC_SERVER) } {
                Ok(d) => d,
                Err(_) => return,
            };
        let shell = Disp(d);
        // Shell.NameSpace(int) returns a Folder object; its Title is a string.
        let folder = shell.call_obj("NameSpace", &[Arg::I32(0x11)]).unwrap(); // ssfDRIVES
        let title = folder.get("Title").unwrap();
        assert!(!BSTR::try_from(&title).unwrap().is_empty());
        assert!(shell.get("NoSuchMemberXyz").is_err());
        // Passing an object argument round-trips through VT_DISPATCH.
        let v = Arg::Disp(&folder.0).to_variant();
        assert!(to_disp(&v).is_ok());
    }
}
