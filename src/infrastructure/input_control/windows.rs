use super::*;
use std::collections::HashSet;
use std::sync::Mutex;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

#[derive(Default)]
struct InputState {
    pressed_buttons: HashSet<MouseButton>,
    pressed_keys: HashSet<u16>,
    pointer_residual_x: f32,
    pointer_residual_y: f32,
    scroll_residual_x: f32,
    scroll_residual_y: f32,
}

pub struct NativeInputControl {
    state: Mutex<InputState>,
    gestures: super::windows_system_gesture::WindowsSystemGesture,
}

impl NativeInputControl {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(InputState::default()),
            gestures: super::windows_system_gesture::WindowsSystemGesture::default(),
        }
    }

    fn send(input: INPUT) -> Result<()> {
        let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
        if sent == 1 {
            Ok(())
        } else {
            Err(Error::InputControl("SendInput failed".into()))
        }
    }

    fn mouse_input(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: u32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn key_input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn set_button(
        state: &mut InputState,
        button: MouseButton,
        down: bool,
        _click_count: u8,
    ) -> Result<()> {
        let flags = match (button, down) {
            (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
            (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
            (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
            (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
            (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
            (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
        };
        Self::send(Self::mouse_input(flags, 0, 0, 0))?;
        if down {
            state.pressed_buttons.insert(button);
        } else {
            state.pressed_buttons.remove(&button);
        }
        Ok(())
    }

    fn set_key(state: &mut InputState, hid_usage: u16, down: bool) -> Result<()> {
        let (vk, extended) = hid_to_windows_vk(hid_usage).ok_or_else(|| {
            Error::InputControl(format!("unsupported HID usage: 0x{hid_usage:02X}"))
        })?;
        let mut flags = if down {
            KEYBD_EVENT_FLAGS(0)
        } else {
            KEYEVENTF_KEYUP
        };
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        Self::send(Self::key_input(vk, 0, flags))?;
        if down {
            state.pressed_keys.insert(hid_usage);
        } else {
            state.pressed_keys.remove(&hid_usage);
        }
        Ok(())
    }

    fn move_pointer(state: &mut InputState, delta_x: f32, delta_y: f32) -> Result<()> {
        let total_x = state.pointer_residual_x + delta_x;
        let total_y = state.pointer_residual_y + delta_y;
        let x = total_x.round() as i32;
        let y = total_y.round() as i32;
        if x != 0 || y != 0 {
            Self::send(Self::mouse_input(MOUSEEVENTF_MOVE, x, y, 0))?;
        }
        state.pointer_residual_x = total_x - x as f32;
        state.pointer_residual_y = total_y - y as f32;
        Ok(())
    }

    fn commit_text(text: &str) -> Result<()> {
        for unit in text.encode_utf16() {
            Self::send(Self::key_input(0, unit, KEYEVENTF_UNICODE))?;
            Self::send(Self::key_input(
                0,
                unit,
                KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
            ))?;
        }
        Ok(())
    }

    fn scroll(state: &mut InputState, delta_x: f32, delta_y: f32) -> Result<()> {
        let total_y = state.scroll_residual_y + delta_y * 12.0;
        let wheel_y = total_y.round() as i32;
        if wheel_y != 0 {
            Self::send(Self::mouse_input(MOUSEEVENTF_WHEEL, 0, 0, wheel_y as u32))?;
        }
        state.scroll_residual_y = total_y - wheel_y as f32;

        let total_x = state.scroll_residual_x + delta_x * 12.0;
        let wheel_x = total_x.round() as i32;
        if wheel_x != 0 {
            Self::send(Self::mouse_input(MOUSEEVENTF_HWHEEL, 0, 0, wheel_x as u32))?;
        }
        state.scroll_residual_x = total_x - wheel_x as f32;
        Ok(())
    }

    fn release_state(state: &mut InputState) {
        for key in state.pressed_keys.clone() {
            let _ = Self::set_key(state, key, false);
        }
        for button in state.pressed_buttons.clone() {
            let _ = Self::set_button(state, button, false, 1);
        }
        state.pressed_keys.clear();
        state.pressed_buttons.clear();
        state.pointer_residual_x = 0.0;
        state.pointer_residual_y = 0.0;
        state.scroll_residual_x = 0.0;
        state.scroll_residual_y = 0.0;
    }

    fn validate_events_sync(events: &[InputEvent]) -> Result<()> {
        for event in events {
            match event {
                InputEvent::SystemGesture(event) => {
                    event
                        .validate_format(1)
                        .map_err(|error| Error::InputControl(error.into()))?;
                }
                InputEvent::PointerMove { delta_x, delta_y }
                | InputEvent::Scroll {
                    delta_x, delta_y, ..
                } if !delta_x.is_finite() || !delta_y.is_finite() => {
                    return Err(Error::InputControl(
                        "input motion contains a non-finite delta".into(),
                    ));
                }
                InputEvent::PointerButton { click_count, .. } if !(1..=3).contains(click_count) => {
                    return Err(Error::InputControl(
                        "pointer click count must be between 1 and 3".into(),
                    ));
                }
                InputEvent::Key { hid_usage, .. } if hid_to_windows_vk(*hid_usage).is_none() => {
                    return Err(Error::InputControl(format!(
                        "unsupported HID usage: 0x{hid_usage:02X}"
                    )));
                }
                InputEvent::TextCommit(text) if text.is_empty() => {
                    return Err(Error::InputControl("text commit is empty".into()));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn apply_events_sync(&self, events: &[InputEvent]) -> Result<()> {
        Self::validate_events_sync(events)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::InputControl("input state lock poisoned".into()))?;
        for event in events {
            match event {
                InputEvent::PointerMove { delta_x, delta_y } => {
                    Self::move_pointer(&mut state, *delta_x, *delta_y)?
                }
                InputEvent::PointerButton {
                    button,
                    down,
                    click_count,
                } => Self::set_button(&mut state, *button, *down, *click_count)?,
                InputEvent::Scroll {
                    delta_x, delta_y, ..
                } => Self::scroll(&mut state, *delta_x, *delta_y)?,
                InputEvent::ScrollGesture { .. } => {}
                InputEvent::SystemGesture(event) => {
                    self.gestures.apply(*event)?;
                }
                InputEvent::Key {
                    hid_usage, down, ..
                } => Self::set_key(&mut state, *hid_usage, *down)?,
                InputEvent::TextCommit(text) => Self::commit_text(text)?,
                InputEvent::ReleaseAll => {
                    self.gestures.cancel();
                    Self::release_state(&mut state);
                }
            }
        }
        Ok(())
    }

    fn release_all_sync(&self) -> Result<()> {
        self.gestures.cancel();
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::InputControl("input state lock poisoned".into()))?;
        Self::release_state(&mut state);
        Ok(())
    }

    fn paste_clipboard_sync(&self, is_text: bool) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::InputControl("input state lock poisoned".into()))?;

        // Start from a known modifier state. This also recovers cleanly if
        // a previous remote-input session ended between key-down/up.
        Self::release_state(&mut state);
        std::thread::sleep(std::time::Duration::from_millis(50));

        let (modifier, key) = if is_text {
            // PasteGroup uses Shift+Insert for text because it is accepted
            // more consistently by Windows text controls.
            (0xE1, 0x49)
        } else {
            // Images and file lists require Ctrl+V in apps such as WeChat.
            (0xE0, 0x19)
        };

        Self::set_key(&mut state, modifier, true)?;
        let paste_result = Self::set_key(&mut state, key, true)
            .and_then(|_| Self::set_key(&mut state, key, false));
        let release_result = Self::set_key(&mut state, modifier, false);
        if paste_result.is_err() || release_result.is_err() {
            Self::release_state(&mut state);
        }
        paste_result.and(release_result)
    }
}

impl Drop for NativeInputControl {
    fn drop(&mut self) {
        let _ = self.release_all_sync();
    }
}

#[async_trait::async_trait]
impl InputControlRepository for NativeInputControl {
    fn supports_system_gestures(&self) -> bool {
        self.gestures.available()
    }

    fn permission_state(&self) -> InputPermissionState {
        InputPermissionState::Granted
    }

    fn open_permission_settings(&self) -> Result<()> {
        Ok(())
    }

    fn validate_events(&self, events: &[InputEvent]) -> Result<()> {
        Self::validate_events_sync(events)
    }

    async fn apply_events(&self, events: &[InputEvent]) -> Result<()> {
        self.apply_events_sync(events)
    }

    async fn paste_clipboard(&self, is_text: bool) -> Result<()> {
        self.paste_clipboard_sync(is_text)
    }

    async fn release_all(&self) -> Result<()> {
        self.release_all_sync()
    }
}

fn hid_to_windows_vk(usage: u16) -> Option<(u16, bool)> {
    if (0x04..=0x1D).contains(&usage) {
        return Some((b'A' as u16 + usage - 0x04, false));
    }
    if (0x1E..=0x26).contains(&usage) {
        return Some((b'1' as u16 + usage - 0x1E, false));
    }
    Some(match usage {
        0x27 => (b'0' as u16, false),
        0x28 => (0x0D, false),
        0x29 => (0x1B, false),
        0x2A => (0x08, false),
        0x2B => (0x09, false),
        0x2C => (0x20, false),
        0x2D => (0xBD, false),
        0x2E => (0xBB, false),
        0x2F => (0xDB, false),
        0x30 => (0xDD, false),
        0x31 => (0xDC, false),
        0x33 => (0xBA, false),
        0x34 => (0xDE, false),
        0x35 => (0xC0, false),
        0x36 => (0xBC, false),
        0x37 => (0xBE, false),
        0x38 => (0xBF, false),
        0x39 => (0x14, false),
        0x3A..=0x45 => (0x70 + usage - 0x3A, false),
        0x49 => (0x2D, true),
        0x4A => (0x24, true),
        0x4B => (0x21, true),
        0x4C => (0x2E, true),
        0x4D => (0x23, true),
        0x4E => (0x22, true),
        0x4F => (0x27, true),
        0x50 => (0x25, true),
        0x51 => (0x28, true),
        0x52 => (0x26, true),
        0xE0 => (0xA2, false),
        0xE1 => (0xA0, false),
        0xE2 => (0xA4, false),
        0xE3 => (0x5B, true),
        0xE4 => (0xA3, true),
        0xE5 => (0xA1, false),
        0xE6 => (0xA5, true),
        0xE7 => (0x5C, true),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::hid_to_windows_vk;

    #[test]
    fn windows_input_constructs_and_releases_without_tokio() {
        let input = super::NativeInputControl::new();
        input.release_all_sync().unwrap();
        assert!(input.state.lock().unwrap().pressed_keys.is_empty());
    }

    #[test]
    fn maps_all_keys_exposed_by_the_mobile_keyboard() {
        let usages = [
            0x04, 0x06, 0x19, 0x1B, 0x1C, 0x1D, 0x28, 0x29, 0x2A, 0x2B, 0x49, 0x4A, 0x4B, 0x4C,
            0x4D, 0x4E, 0x4F, 0x50, 0x51, 0x52, 0xE0, 0xE1, 0xE2, 0xE3,
        ];

        assert!(usages
            .into_iter()
            .all(|usage| hid_to_windows_vk(usage).is_some()));
        assert!(hid_to_windows_vk(0).is_none());
    }
}
