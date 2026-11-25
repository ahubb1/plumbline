//! The Windows printers, through GDI: the list (`EnumPrintersW`), the paper of one
//! (`GetDeviceCaps` on its device context) and a [`PrintSink`] that draws each band with
//! `StretchDIBits` between `StartDocW`/`StartPage` and `EndPage`/`EndDoc`.
//!
//! Only the window's start-up code gives the app these printers
//! ([`AppState::set_printers`](crate::AppState::set_printers)); nothing in the library makes
//! one by itself, so tests, which use [`crate::print::FakePrinters`] or nothing, can never
//! start a real print job.

use std::os::windows::ffi::OsStrExt;

use pl_doc::pl_render::{
    Band, BandFormat, JobStart, Paper, PrintSink, PrintableArea, SheetStart, SinkError,
};
use windows_sys::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, GetDeviceCaps, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HDC, HORZRES, LOGPIXELSX, LOGPIXELSY, PHYSICALHEIGHT, PHYSICALOFFSETX,
    PHYSICALOFFSETY, PHYSICALWIDTH, SRCCOPY, VERTRES,
};
use windows_sys::Win32::Graphics::Printing::{
    EnumPrintersW, GetDefaultPrinterW, PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL,
    PRINTER_INFO_4W,
};
use windows_sys::Win32::Storage::Xps::{AbortDoc, EndDoc, EndPage, StartDocW, StartPage, DOCINFOW};

use crate::print::{PrinterInfo, Printers};

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// A NUL-terminated wide string at `p`.
///
/// # Safety
/// `p` must be null or point at a NUL-terminated UTF-16 string.
unsafe fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: the caller promises a terminator.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` units were just read.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

/// The printers of this machine, through GDI.
#[derive(Debug, Default)]
pub struct WinPrinters;

fn default_printer() -> Option<String> {
    let mut len = 0u32;
    // SAFETY: a null buffer with a length asks for the size needed.
    unsafe { GetDefaultPrinterW(std::ptr::null_mut(), &mut len) };
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: the buffer holds `len` units.
    let ok = unsafe { GetDefaultPrinterW(buf.as_mut_ptr(), &mut len) };
    // SAFETY: on success the buffer holds a terminated name.
    (ok != 0).then(|| unsafe { from_wide(buf.as_ptr()) })
}

/// A device context of the printer, deleted when dropped.
struct Dc(HDC);

impl Dc {
    fn of(printer: &str) -> Result<Dc, String> {
        let name = wide(printer);
        // SAFETY: the name is terminated; the other arguments may be null.
        let hdc = unsafe {
            CreateDCW(
                std::ptr::null(),
                name.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if hdc.is_null() {
            Err(format!("Windows could not open the printer {printer}"))
        } else {
            Ok(Dc(hdc))
        }
    }

    fn cap(&self, index: u32) -> i32 {
        // SAFETY: the context is alive.
        unsafe { GetDeviceCaps(self.0, index as i32) }
    }

    fn paper(&self) -> Result<Paper, String> {
        let dpi = [self.cap(LOGPIXELSX) as f64, self.cap(LOGPIXELSY) as f64];
        let size = [self.cap(PHYSICALWIDTH), self.cap(PHYSICALHEIGHT)];
        let offset = [self.cap(PHYSICALOFFSETX), self.cap(PHYSICALOFFSETY)];
        let printable = [self.cap(HORZRES), self.cap(VERTRES)];
        if size.iter().chain(&printable).any(|v| *v <= 0) || dpi.iter().any(|d| *d <= 0.0) {
            return Err("the printer did not report its paper".into());
        }
        let paper = Paper {
            dpi,
            size: [size[0] as u32, size[1] as u32],
            printable: PrintableArea {
                x: offset[0].max(0) as u32,
                y: offset[1].max(0) as u32,
                width: printable[0] as u32,
                height: printable[1] as u32,
            },
        };
        paper.validate().map_err(|e| e.to_string())?;
        Ok(paper)
    }
}

impl Drop for Dc {
    fn drop(&mut self) {
        // SAFETY: the context was made by CreateDCW and is deleted once.
        unsafe { DeleteDC(self.0) };
    }
}

impl Printers for WinPrinters {
    fn list(&self) -> Vec<PrinterInfo> {
        let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
        let (mut needed, mut count) = (0u32, 0u32);
        // SAFETY: a null buffer of size 0 asks for the size needed.
        unsafe {
            EnumPrintersW(
                flags,
                std::ptr::null(),
                4,
                std::ptr::null_mut(),
                0,
                &mut needed,
                &mut count,
            )
        };
        if needed == 0 {
            return Vec::new();
        }
        // PRINTER_INFO_4W holds pointers: the buffer must be aligned for them.
        let words = (needed as usize).div_ceil(std::mem::size_of::<u64>());
        let mut buf = vec![0u64; words];
        // SAFETY: the buffer is `needed` bytes or more and suitably aligned.
        let ok = unsafe {
            EnumPrintersW(
                flags,
                std::ptr::null(),
                4,
                buf.as_mut_ptr().cast(),
                needed,
                &mut needed,
                &mut count,
            )
        };
        if ok == 0 {
            return Vec::new();
        }
        let default = default_printer();
        // SAFETY: on success the buffer starts with `count` PRINTER_INFO_4W records.
        let infos = unsafe {
            std::slice::from_raw_parts(buf.as_ptr().cast::<PRINTER_INFO_4W>(), count as usize)
        };
        let mut list: Vec<PrinterInfo> = infos
            .iter()
            .map(|info| {
                // SAFETY: the names point into the same buffer and are terminated.
                let name = unsafe { from_wide(info.pPrinterName) };
                PrinterInfo {
                    default: default.as_deref() == Some(name.as_str()),
                    name,
                }
            })
            .filter(|p| !p.name.is_empty())
            .collect();
        list.sort_by_key(|p| !p.default);
        list
    }

    fn paper(&self, printer: &str) -> Result<Paper, String> {
        Dc::of(printer)?.paper()
    }

    fn open(&self, printer: &str, title: &str) -> Result<Box<dyn PrintSink + Send>, String> {
        Ok(Box::new(GdiSink {
            dc: Dc::of(printer)?,
            title: wide(title),
            started: false,
        }))
    }
}

/// A print job on a GDI printer: one band at a time with `StretchDIBits`.
struct GdiSink {
    dc: Dc,
    title: Vec<u16>,
    started: bool,
}

// SAFETY: the device context is used by one thread at a time (the print worker owns the
// sink); GDI printer contexts are not tied to the thread that made them.
unsafe impl Send for GdiSink {}

fn failed(what: &str) -> SinkError {
    SinkError::Device(format!(
        "{what} failed ({})",
        std::io::Error::last_os_error()
    ))
}

impl PrintSink for GdiSink {
    fn begin_job(&mut self, _job: &JobStart<'_>) -> Result<(), SinkError> {
        let info = DOCINFOW {
            cbSize: std::mem::size_of::<DOCINFOW>() as i32,
            lpszDocName: self.title.as_ptr(),
            lpszOutput: std::ptr::null(),
            lpszDatatype: std::ptr::null(),
            fwType: 0,
        };
        // SAFETY: the context is alive and the info and its title outlive the call.
        if unsafe { StartDocW(self.dc.0, &info) } <= 0 {
            return Err(failed("StartDoc"));
        }
        self.started = true;
        Ok(())
    }

    fn begin_page(&mut self, _sheet: &SheetStart) -> Result<(), SinkError> {
        // SAFETY: the context is alive and a document has been started.
        if unsafe { StartPage(self.dc.0) } <= 0 {
            return Err(failed("StartPage"));
        }
        Ok(())
    }

    fn band(&mut self, band: &Band<'_>) -> Result<(), SinkError> {
        let bits = match band.format {
            BandFormat::Bgra8 => 32,
            BandFormat::Bgr8 => 24,
            BandFormat::Rgba8 => {
                return Err(SinkError::Device(
                    "GDI takes BGR rows; the job asked for RGBA".into(),
                ))
            }
        };
        let header = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: band.width as i32,
            // Negative: the rows are top-down.
            biHeight: -(band.height as i32),
            biPlanes: 1,
            biBitCount: bits,
            biCompression: BI_RGB,
            biSizeImage: (band.stride * band.height as usize) as u32,
            ..Default::default()
        };
        let info = BITMAPINFO {
            bmiHeader: header,
            ..Default::default()
        };
        // SAFETY: `data` holds `height` rows of `stride` bytes as the header says; the
        // context is alive inside a page.
        let lines = unsafe {
            StretchDIBits(
                self.dc.0,
                band.x as i32,
                band.y as i32,
                band.width as i32,
                band.height as i32,
                0,
                0,
                band.width as i32,
                band.height as i32,
                band.data.as_ptr().cast(),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            )
        };
        if lines <= 0 {
            return Err(failed("StretchDIBits"));
        }
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), SinkError> {
        // SAFETY: the context is alive inside a page.
        if unsafe { EndPage(self.dc.0) } <= 0 {
            return Err(failed("EndPage"));
        }
        Ok(())
    }

    fn end_job(&mut self) -> Result<(), SinkError> {
        self.started = false;
        // SAFETY: the context is alive and a document was started.
        if unsafe { EndDoc(self.dc.0) } <= 0 {
            return Err(failed("EndDoc"));
        }
        Ok(())
    }

    fn abort(&mut self) {
        if std::mem::take(&mut self.started) {
            // SAFETY: the context is alive and a document was started.
            unsafe { AbortDoc(self.dc.0) };
        }
    }
}
