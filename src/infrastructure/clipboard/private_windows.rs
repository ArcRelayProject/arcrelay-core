//! Windows private writes and compare-and-clear hold the native clipboard lock throughout.
use super::*;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND},
        System::{DataExchange::*, Memory::*},
    },
};
struct Board(HWND);
impl Board {
    fn open() -> Result<Self> {
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                windows::core::w!("STATIC"),
                windows::core::w!("ArcRelay private clipboard"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                None,
                None,
                None,
            )
        }
        .map_err(|_| Error::Clipboard("clipboard owner unavailable".into()))?;
        for _ in 0..5 {
            if unsafe { OpenClipboard(window) }.is_ok() {
                return Ok(Self(window));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        unsafe {
            let _ = DestroyWindow(window);
        }
        Err(Error::Clipboard("clipboard is busy".into()))
    }
}
impl Drop for Board {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
            let _ = DestroyWindow(self.0);
        }
    }
}
fn format(name: &str) -> Result<u32> {
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let id = unsafe { RegisterClipboardFormatW(PCWSTR(name.as_ptr())) };
    if id == 0 {
        Err(Error::Clipboard(
            "private clipboard format unavailable".into(),
        ))
    } else {
        Ok(id)
    }
}
fn put(id: u32, bytes: &[u8]) -> Result<()> {
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))
            .map_err(|_| Error::Clipboard("clipboard allocation failed".into()))?;
        let pointer = GlobalLock(memory);
        if pointer.is_null() {
            let _ = GlobalFree(memory);
            return Err(Error::Clipboard("clipboard allocation failed".into()));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast(), bytes.len());
        let _ = GlobalUnlock(memory);
        if SetClipboardData(id, HANDLE(memory.0)).is_err() {
            let _ = GlobalFree(memory);
            return Err(Error::Clipboard("clipboard write failed".into()));
        }
        Ok(())
    }
}
pub(super) fn write(text: &str, receipt: &str) -> Result<String> {
    let _board = Board::open()?;
    unsafe { EmptyClipboard() }.map_err(|_| Error::Clipboard("clipboard clear failed".into()))?;
    // Required markers precede plaintext; failure leaves no plaintext representation.
    put(format(PRIVATE_MARKER)?, receipt.as_bytes())?;
    put(format("CanIncludeInClipboardHistory")?, &0u32.to_le_bytes())?;
    put(format("CanUploadToCloudClipboard")?, &0u32.to_le_bytes())?;
    put(
        format("ExcludeClipboardContentFromMonitorProcessing")?,
        &[0],
    )?;
    let text = zeroize::Zeroizing::new(
        text.encode_utf16()
            .chain(Some(0))
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    put(13, &text)?; // CF_UNICODETEXT
    Ok(format!("{}:{}", receipt, unsafe {
        GetClipboardSequenceNumber()
    }))
}
pub(super) fn clear(receipt: &str) -> Result<bool> {
    let Some((token, expected)) = receipt.rsplit_once(':') else {
        return Ok(false);
    };
    let _board = Board::open()?;
    if expected.parse::<u32>().ok() != Some(unsafe { GetClipboardSequenceNumber() }) {
        return Ok(false);
    }
    unsafe {
        let Ok(handle) = GetClipboardData(format(PRIVATE_MARKER)?) else {
            return Ok(false);
        };
        let memory = HGLOBAL(handle.0);
        let size = GlobalSize(memory);
        let data = GlobalLock(memory);
        if data.is_null() {
            return Ok(false);
        }
        let matches = size >= token.len()
            && std::slice::from_raw_parts(data.cast::<u8>(), token.len()) == token.as_bytes();
        let _ = GlobalUnlock(memory);
        if !matches {
            return Ok(false);
        }
        EmptyClipboard().map_err(|_| Error::Clipboard("clipboard clear failed".into()))?;
    }
    Ok(true)
}
