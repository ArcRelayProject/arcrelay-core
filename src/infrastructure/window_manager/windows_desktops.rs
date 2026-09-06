//! Explorer desktop identity and the supported Win+Ctrl+Arrow switch path.
//! Registry layout is undocumented: malformed/unavailable data disables the
//! desktop projection instead of guessing desktop numbers or COM vtables.
use std::collections::{HashMap, HashSet};

type DesktopGuid = [u8; 16];
const MAX_DESKTOPS: usize = 256;

fn parse_desktop_ids(bytes: &[u8]) -> Option<Vec<DesktopGuid>> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(16) || bytes.len() > MAX_DESKTOPS * 16 {
        return None;
    }
    let ids: Vec<DesktopGuid> = bytes.as_chunks::<16>().0.to_vec();
    let mut seen = HashSet::new();
    ids.iter()
        .all(|id| *id != [0; 16] && seen.insert(*id))
        .then_some(ids)
}

#[derive(Default)]
struct DesktopIds {
    ids: HashMap<DesktopGuid, u64>,
    next: u64,
}

impl DesktopIds {
    fn get(&mut self, guid: DesktopGuid) -> u64 {
        *self.ids.entry(guid).or_insert_with(|| {
            self.next += 1;
            self.next
        })
    }
}

#[cfg(target_os = "windows")]
pub(crate) use native::*;

#[cfg(target_os = "windows")]
mod native {
    use super::*;
    use crate::domain::window_manager::SpaceInfo;
    use crate::error::{Error, Result};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    use windows::core::{GUID, PCWSTR};
    use windows::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, REG_ROUTINE_FLAGS, RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    const ROOT: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\VirtualDesktops";
    static IDS: OnceLock<Mutex<DesktopIds>> = OnceLock::new();
    static SWITCH: Mutex<()> = Mutex::new(());

    pub(crate) fn registry_value(
        path: &str,
        name: &str,
        flags: REG_ROUTINE_FLAGS,
    ) -> Option<Vec<u8>> {
        let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // A fixed bound also handles registry values changing between reads.
        let mut bytes = vec![0u8; MAX_DESKTOPS * 16];
        let mut size = bytes.len() as u32;
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                PCWSTR(name.as_ptr()),
                flags,
                None,
                Some(bytes.as_mut_ptr().cast()),
                Some(&mut size),
            )
            .ok()
            .ok()?;
        }
        bytes.truncate(size as usize);
        Some(bytes)
    }

    fn session_path() -> Option<String> {
        let mut id = 0;
        unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id) }.ok()?;
        Some(format!(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\SessionInfo\{id}\VirtualDesktops"
        ))
    }

    fn guid_bytes(id: GUID) -> DesktopGuid {
        let mut bytes = [0; 16];
        bytes[..4].copy_from_slice(&id.data1.to_le_bytes());
        bytes[4..6].copy_from_slice(&id.data2.to_le_bytes());
        bytes[6..8].copy_from_slice(&id.data3.to_le_bytes());
        bytes[8..].copy_from_slice(&id.data4);
        bytes
    }

    fn guid(id: DesktopGuid) -> GUID {
        GUID::from_values(
            u32::from_le_bytes(id[..4].try_into().unwrap()),
            u16::from_le_bytes(id[4..6].try_into().unwrap()),
            u16::from_le_bytes(id[6..8].try_into().unwrap()),
            id[8..].try_into().unwrap(),
        )
    }

    struct Apartment(bool);
    impl Apartment {
        fn enter() -> Option<Self> {
            let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if result.is_ok() {
                Some(Self(true))
            } else if result == RPC_E_CHANGED_MODE {
                Some(Self(false))
            } else {
                None
            }
        }
    }
    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    // COM and its apartment stay on the same blocking worker for each sample.
    pub(crate) struct DesktopSnapshot {
        pub spaces: Vec<SpaceInfo>,
        ids: Vec<DesktopGuid>,
        manager: Option<IVirtualDesktopManager>,
        _apartment: Option<Apartment>,
    }

    impl DesktopSnapshot {
        pub fn read() -> Self {
            let apartment = Apartment::enter();
            let manager: Option<IVirtualDesktopManager> = apartment.as_ref().and_then(|_| unsafe {
                CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL).ok()
            });
            let session = session_path();
            let ids = registry_value(ROOT, "VirtualDesktopIDs", RRF_RT_REG_BINARY)
                .and_then(|bytes| parse_desktop_ids(&bytes))
                .or_else(|| {
                    session
                        .as_ref()
                        .and_then(|path| {
                            registry_value(path, "VirtualDesktopIDs", RRF_RT_REG_BINARY)
                        })
                        .and_then(|bytes| parse_desktop_ids(&bytes))
                })
                .unwrap_or_default();
            let from_registry = |path: &str| {
                registry_value(path, "CurrentVirtualDesktop", RRF_RT_REG_BINARY)
                    .and_then(|bytes| <DesktopGuid>::try_from(bytes).ok())
                    .filter(|id| ids.contains(id))
            };
            let active = manager.as_ref().and_then(|manager| unsafe {
                let hwnd = GetForegroundWindow();
                manager
                    .IsWindowOnCurrentVirtualDesktop(hwnd)
                    .ok()
                    .filter(|value| value.as_bool())?;
                manager
                    .GetWindowDesktopId(hwnd)
                    .ok()
                    .map(guid_bytes)
                    .filter(|id| ids.contains(id))
            });
            // Registry includes empty desktops; prefer it over a pinned foreground window.
            let active = from_registry(ROOT)
                .or_else(|| session.as_deref().and_then(from_registry))
                .or(active);
            let mut identities = IDS
                .get_or_init(|| Mutex::new(DesktopIds::default()))
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let spaces = ids
                .iter()
                .enumerate()
                .map(|(index, id)| {
                    let label = registry_value(
                        &format!(r"{ROOT}\Desktops\{{{:?}}}", guid(*id)),
                        "Name",
                        RRF_RT_REG_SZ,
                    )
                    .filter(|bytes| bytes.len().is_multiple_of(2))
                    .map(|bytes| {
                        String::from_utf16_lossy(
                            &bytes
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
                                .take_while(|unit| *unit != 0)
                                .collect::<Vec<_>>(),
                        )
                    })
                    .filter(|label| !label.trim().is_empty())
                    .unwrap_or_else(|| format!("Desktop {}", index + 1));
                    SpaceInfo {
                        space_id: identities.get(*id),
                        label,
                        is_active: active == Some(*id),
                    }
                })
                .collect();
            Self {
                spaces,
                ids,
                manager,
                _apartment: apartment,
            }
        }

        pub fn window_space(&self, hwnd: HWND) -> Option<u64> {
            let manager = self.manager.as_ref()?;
            let id = unsafe { manager.GetWindowDesktopId(hwnd) }
                .ok()
                .map(guid_bytes)?;
            let index = self.ids.iter().position(|candidate| *candidate == id)?;
            Some(self.spaces[index].space_id)
        }

        pub fn window_is_current(&self, hwnd: HWND) -> bool {
            self.manager
                .as_ref()
                .and_then(|manager| unsafe { manager.IsWindowOnCurrentVirtualDesktop(hwnd).ok() })
                .is_some_and(|value| value.as_bool())
        }

        fn current_id(&self) -> Option<u64> {
            self.spaces
                .iter()
                .find(|space| space.is_active)
                .map(|space| space.space_id)
        }
    }

    pub(crate) fn switch_space(target: u64) -> Result<()> {
        let _guard = SWITCH.lock().unwrap_or_else(|error| error.into_inner());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let snapshot = DesktopSnapshot::read();
            let target_index = snapshot
                .spaces
                .iter()
                .position(|space| space.space_id == target)
                .ok_or_else(|| Error::NotFound(format!("desktop {target} no longer exists")))?;
            let current_index = snapshot
                .spaces
                .iter()
                .position(|space| space.is_active)
                .ok_or_else(|| {
                    Error::NotSupported("Windows active desktop is unavailable".into())
                })?;
            if current_index == target_index {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::InputControl("desktop switch timed out".into()));
            }
            let current = snapshot.current_id();
            send_switch_chord(target_index > current_index)?;
            let step_deadline = (Instant::now() + Duration::from_secs(2)).min(deadline);
            loop {
                std::thread::sleep(Duration::from_millis(40));
                let next = DesktopSnapshot::read();
                if next.current_id().is_some() && next.current_id() != current {
                    break;
                }
                if Instant::now() >= step_deadline {
                    return Err(Error::InputControl(
                        "Windows did not confirm the desktop switch".into(),
                    ));
                }
            }
            // Explorer can publish its ID before the switch animation completes.
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn send_switch_chord(right: bool) -> Result<()> {
        let arrow = if right { VK_RIGHT } else { VK_LEFT };
        // Preserve physically/remote-held modifiers. Never release a key we did not press.
        let held = |key: VIRTUAL_KEY| unsafe { GetAsyncKeyState(key.0 as i32) } < 0;
        if [VK_MENU, VK_SHIFT, arrow].into_iter().any(held) {
            return Err(Error::InputControl(
                "release Alt, Shift and arrow keys before switching desktops".into(),
            ));
        }
        let mut pressed = Vec::new();
        if !held(VK_LWIN) && !held(VK_RWIN) {
            pressed.push(VK_LWIN);
        }
        if !held(VK_CONTROL) {
            pressed.push(VK_CONTROL);
        }
        pressed.push(arrow);
        let key = |vk, up| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    dwFlags: (if up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    }) | (if vk == arrow || vk == VK_LWIN {
                        KEYEVENTF_EXTENDEDKEY
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    }),
                    dwExtraInfo: 0x4152_4349_4e50_5554,
                    ..Default::default()
                },
            },
        };
        let inputs: Vec<_> = pressed
            .iter()
            .map(|vk| key(*vk, false))
            .chain(pressed.iter().rev().map(|vk| key(*vk, true)))
            .collect();
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            let release: Vec<_> = pressed.iter().rev().map(|vk| key(*vk, true)).collect();
            unsafe { SendInput(&release, std::mem::size_of::<INPUT>() as i32) };
            return Err(Error::InputControl(
                "Windows rejected the desktop shortcut".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_registry_rejects_partial_duplicate_and_null_guids() {
        assert!(parse_desktop_ids(&[]).is_none());
        assert!(parse_desktop_ids(&[1; 17]).is_none());
        assert!(parse_desktop_ids(&[0; 16]).is_none());
        assert!(parse_desktop_ids(&[1; 32]).is_none());
        assert!(parse_desktop_ids(&vec![1; 16 * (MAX_DESKTOPS + 1)]).is_none());
        assert_eq!(
            parse_desktop_ids(&[[1; 16], [2; 16]].concat()),
            Some(vec![[1; 16], [2; 16]])
        );
    }
    #[test]
    fn desktop_identity_survives_reorder_and_never_truncates_guids() {
        let mut ids = DesktopIds::default();
        let first = [1; 16];
        let mut second = first;
        second[15] = 2;
        let a = ids.get(first);
        let b = ids.get(second);
        assert_ne!(a, 0);
        assert_ne!(a, b);
        assert_eq!((ids.get(second), ids.get(first)), (b, a));
        assert!(ids.get([3; 16]) > b);
    }
}
