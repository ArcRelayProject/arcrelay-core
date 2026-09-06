use super::macos_system_gesture;
use super::*;
use crate::domain::input_control::SystemGestureSequence;
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::CFString;
use dispatch::{Queue, QueueAttribute, QueuePriority};
use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

fn offset_point(point: CGPoint, delta_x: f32, delta_y: f32) -> CGPoint {
    CGPoint {
        x: point.x + f64::from(delta_x),
        y: point.y + f64::from(delta_y),
    }
}

type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;

const K_CG_HID_EVENT_TAP: u32 = 0;
const K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION: i32 = 0;
const K_CG_EVENT_SOURCE_USER_DATA: u32 = 42;
const K_CG_SCROLL_EVENT_UNIT_PIXEL: u32 = 0;
const K_CG_MOUSE_BUTTON_LEFT: u32 = 0;
const K_CG_MOUSE_BUTTON_RIGHT: u32 = 1;
const K_CG_MOUSE_BUTTON_CENTER: u32 = 2;
const K_CG_EVENT_LEFT_MOUSE_DOWN: u32 = 1;
const K_CG_EVENT_LEFT_MOUSE_UP: u32 = 2;
const K_CG_EVENT_RIGHT_MOUSE_DOWN: u32 = 3;
const K_CG_EVENT_RIGHT_MOUSE_UP: u32 = 4;
const K_CG_EVENT_MOUSE_MOVED: u32 = 5;
const K_CG_EVENT_LEFT_MOUSE_DRAGGED: u32 = 6;
const K_CG_EVENT_RIGHT_MOUSE_DRAGGED: u32 = 7;
const K_CG_EVENT_OTHER_MOUSE_DOWN: u32 = 25;
const K_CG_EVENT_OTHER_MOUSE_UP: u32 = 26;
const K_CG_EVENT_OTHER_MOUSE_DRAGGED: u32 = 27;
const K_CG_MOUSE_EVENT_CLICK_STATE: u32 = 1;
const K_CG_MOUSE_EVENT_DELTA_X: u32 = 4;
const K_CG_MOUSE_EVENT_DELTA_Y: u32 = 5;
const K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS: u32 = 88;
const K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1: u32 = 93;
const K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2: u32 = 94;
const K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1: u32 = 96;
const K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2: u32 = 97;
const K_CG_SCROLL_WHEEL_EVENT_SCROLL_PHASE: u32 = 99;
const K_CG_SCROLL_WHEEL_EVENT_SCROLL_COUNT: u32 = 100;
const K_CG_SCROLL_WHEEL_EVENT_MOMENTUM_PHASE: u32 = 123;
const K_CG_SCROLL_PHASE_BEGAN: i64 = 1;
const K_CG_SCROLL_PHASE_CHANGED: i64 = 2;
const K_CG_SCROLL_PHASE_ENDED: i64 = 4;
const K_CG_SCROLL_PHASE_CANCELLED: i64 = 8;
const K_CG_SCROLL_PHASE_MAY_BEGIN: i64 = 128;
const K_CG_MOMENTUM_SCROLL_PHASE_BEGIN: i64 = 1;
const K_CG_MOMENTUM_SCROLL_PHASE_CONTINUE: i64 = 2;
const K_CG_MOMENTUM_SCROLL_PHASE_END: i64 = 3;
const PERMISSION_RECHECK_INTERVAL: Duration = Duration::from_secs(1);

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    fn CGEventSourceCreate(state_id: i32) -> CGEventSourceRef;
    fn CGEventCreate(source: *const c_void) -> CGEventRef;
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    fn CGEventCreateMouseEvent(
        source: *const c_void,
        mouse_type: u32,
        mouse_cursor_position: CGPoint,
        mouse_button: u32,
    ) -> CGEventRef;
    fn CGEventCreateScrollWheelEvent2(
        source: *const c_void,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> CGEventRef;
    fn CGEventCreateKeyboardEvent(
        source: *const c_void,
        virtual_key: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventSetDoubleValueField(event: CGEventRef, field: u32, value: f64);
    fn CGEventKeyboardSetUnicodeString(
        event: CGEventRef,
        string_length: usize,
        unicode_string: *const u16,
    );
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CFRelease(value: *const c_void);
}

unsafe fn mark_synthetic_event(event: CGEventRef) {
    CGEventSetIntegerValueField(
        event,
        K_CG_EVENT_SOURCE_USER_DATA,
        macos_system_gesture::EVENT_TAG,
    );
}

unsafe fn post_synthetic_event(event: CGEventRef) {
    mark_synthetic_event(event);
    CGEventPost(K_CG_HID_EVENT_TAP, event);
    CFRelease(event);
}

struct InputState {
    event_source: usize,
    pressed_buttons: HashSet<MouseButton>,
    pressed_keys: HashSet<u16>,
    scroll_gesture_active: bool,
    scroll_phase_began_pending: bool,
    scroll_momentum_active: bool,
    momentum_phase_began_pending: bool,
    system_gesture: SystemGestureSequence,
    permission_granted: bool,
    permission_checked_at: Option<Instant>,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            event_source: unsafe {
                CGEventSourceCreate(K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION) as usize
            },
            pressed_buttons: HashSet::new(),
            pressed_keys: HashSet::new(),
            scroll_gesture_active: false,
            scroll_phase_began_pending: false,
            scroll_momentum_active: false,
            momentum_phase_began_pending: false,
            system_gesture: SystemGestureSequence::default(),
            permission_granted: false,
            permission_checked_at: None,
        }
    }
}

impl InputState {
    fn event_source(&self) -> *const c_void {
        self.event_source as *const c_void
    }
}

impl Drop for InputState {
    fn drop(&mut self) {
        if self.event_source != 0 {
            unsafe { CFRelease(self.event_source()) };
        }
    }
}

pub struct NativeInputControl {
    state: Arc<Mutex<InputState>>,
    input_queue: Arc<Queue>,
}

impl NativeInputControl {
    pub fn new() -> Self {
        let high_priority_queue = Queue::global(QueuePriority::High);
        Self {
            state: Arc::new(Mutex::new(InputState::default())),
            input_queue: Arc::new(Queue::with_target_queue(
                "com.arcrelay.remote-input",
                QueueAttribute::Serial,
                &high_priority_queue,
            )),
        }
    }

    fn current_location(state: &InputState) -> Result<CGPoint> {
        unsafe {
            let event = CGEventCreate(state.event_source());
            if event.is_null() {
                return Err(Error::InputControl("CGEventCreate returned null".into()));
            }
            let point = CGEventGetLocation(event);
            CFRelease(event);
            Ok(point)
        }
    }

    fn post_mouse(
        state: &InputState,
        mouse_type: u32,
        button: u32,
        point: CGPoint,
        click_count: u8,
        delta: Option<(i32, i32)>,
    ) -> Result<()> {
        unsafe {
            let event = CGEventCreateMouseEvent(state.event_source(), mouse_type, point, button);
            if event.is_null() {
                return Err(Error::InputControl(
                    "CGEventCreateMouseEvent returned null".into(),
                ));
            }
            if click_count > 0 {
                CGEventSetIntegerValueField(
                    event,
                    K_CG_MOUSE_EVENT_CLICK_STATE,
                    i64::from(click_count),
                );
            }
            if let Some((delta_x, delta_y)) = delta {
                CGEventSetIntegerValueField(event, K_CG_MOUSE_EVENT_DELTA_X, i64::from(delta_x));
                CGEventSetIntegerValueField(event, K_CG_MOUSE_EVENT_DELTA_Y, i64::from(delta_y));
            }
            post_synthetic_event(event);
            Ok(())
        }
    }

    fn move_pointer(state: &mut InputState, delta_x: f32, delta_y: f32) -> Result<()> {
        if delta_x == 0.0 && delta_y == 0.0 {
            return Ok(());
        }
        let current = Self::current_location(state)?;
        let next = offset_point(current, delta_x, delta_y);
        // Quartz cursor coordinates carry sub-point precision. Keep the
        // floating-point target for the actual cursor move instead of
        // quantizing every touch sample to a whole display point. The
        // integer delta fields remain useful metadata for applications
        // that inspect the synthetic event.
        let x = delta_x.round() as i32;
        let y = delta_y.round() as i32;
        let (event_type, button) = if state.pressed_buttons.contains(&MouseButton::Left) {
            (K_CG_EVENT_LEFT_MOUSE_DRAGGED, K_CG_MOUSE_BUTTON_LEFT)
        } else if state.pressed_buttons.contains(&MouseButton::Right) {
            (K_CG_EVENT_RIGHT_MOUSE_DRAGGED, K_CG_MOUSE_BUTTON_RIGHT)
        } else if state.pressed_buttons.contains(&MouseButton::Middle) {
            (K_CG_EVENT_OTHER_MOUSE_DRAGGED, K_CG_MOUSE_BUTTON_CENTER)
        } else {
            (K_CG_EVENT_MOUSE_MOVED, K_CG_MOUSE_BUTTON_LEFT)
        };
        Self::post_mouse(state, event_type, button, next, 0, Some((x, y)))?;
        Ok(())
    }

    fn set_button(
        state: &mut InputState,
        button: MouseButton,
        down: bool,
        click_count: u8,
    ) -> Result<()> {
        let point = Self::current_location(state)?;
        let (down_type, up_type, native_button) = match button {
            MouseButton::Left => (
                K_CG_EVENT_LEFT_MOUSE_DOWN,
                K_CG_EVENT_LEFT_MOUSE_UP,
                K_CG_MOUSE_BUTTON_LEFT,
            ),
            MouseButton::Right => (
                K_CG_EVENT_RIGHT_MOUSE_DOWN,
                K_CG_EVENT_RIGHT_MOUSE_UP,
                K_CG_MOUSE_BUTTON_RIGHT,
            ),
            MouseButton::Middle => (
                K_CG_EVENT_OTHER_MOUSE_DOWN,
                K_CG_EVENT_OTHER_MOUSE_UP,
                K_CG_MOUSE_BUTTON_CENTER,
            ),
        };
        Self::post_mouse(
            state,
            if down { down_type } else { up_type },
            native_button,
            point,
            click_count,
            None,
        )?;
        if down {
            state.pressed_buttons.insert(button);
        } else {
            state.pressed_buttons.remove(&button);
        }
        Ok(())
    }

    fn post_scroll(
        state: &InputState,
        delta_x: f32,
        delta_y: f32,
        scroll_phase: i64,
        momentum_phase: i64,
    ) -> Result<()> {
        unsafe {
            let event = CGEventCreateScrollWheelEvent2(
                state.event_source(),
                K_CG_SCROLL_EVENT_UNIT_PIXEL,
                2,
                delta_y.round() as i32,
                delta_x.round() as i32,
                0,
            );
            if event.is_null() {
                return Err(Error::InputControl(
                    "CGEventCreateScrollWheelEvent2 returned null".into(),
                ));
            }
            CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS, 1);
            CGEventSetIntegerValueField(
                event,
                K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1,
                (f64::from(delta_y) * 65_536.0).round() as i64,
            );
            CGEventSetIntegerValueField(
                event,
                K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2,
                (f64::from(delta_x) * 65_536.0).round() as i64,
            );
            CGEventSetDoubleValueField(
                event,
                K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1,
                f64::from(delta_y),
            );
            CGEventSetDoubleValueField(
                event,
                K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2,
                f64::from(delta_x),
            );
            CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_SCROLL_COUNT, 1);
            CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_SCROLL_PHASE, scroll_phase);
            CGEventSetIntegerValueField(
                event,
                K_CG_SCROLL_WHEEL_EVENT_MOMENTUM_PHASE,
                momentum_phase,
            );
            post_synthetic_event(event);
        }
        Ok(())
    }

    fn scroll(state: &mut InputState, delta_x: f32, delta_y: f32) -> Result<()> {
        let scroll_phase = if state.scroll_gesture_active {
            if state.scroll_phase_began_pending {
                state.scroll_phase_began_pending = false;
                K_CG_SCROLL_PHASE_BEGAN
            } else {
                K_CG_SCROLL_PHASE_CHANGED
            }
        } else {
            0
        };
        let momentum_phase = if state.scroll_momentum_active {
            if state.momentum_phase_began_pending {
                state.momentum_phase_began_pending = false;
                K_CG_MOMENTUM_SCROLL_PHASE_BEGIN
            } else {
                K_CG_MOMENTUM_SCROLL_PHASE_CONTINUE
            }
        } else {
            0
        };
        Self::post_scroll(state, delta_x, delta_y, scroll_phase, momentum_phase)
    }

    fn set_scroll_gesture_phase(state: &mut InputState, phase: ScrollGesturePhase) -> Result<()> {
        match phase {
            ScrollGesturePhase::Began => {
                if state.scroll_momentum_active {
                    Self::post_scroll(state, 0.0, 0.0, 0, K_CG_MOMENTUM_SCROLL_PHASE_END)?;
                }
                state.scroll_momentum_active = false;
                state.momentum_phase_began_pending = false;
                // Native trackpads announce a possible gesture before the
                // first non-zero Began sample. Safari uses this phase to
                // prepare its interactive history swipe animation.
                Self::post_scroll(state, 0.0, 0.0, K_CG_SCROLL_PHASE_MAY_BEGIN, 0)?;
                state.scroll_gesture_active = true;
                state.scroll_phase_began_pending = true;
            }
            ScrollGesturePhase::Ended => {
                if state.scroll_gesture_active {
                    Self::post_scroll(state, 0.0, 0.0, K_CG_SCROLL_PHASE_ENDED, 0)?;
                }
                state.scroll_gesture_active = false;
                state.scroll_phase_began_pending = false;
            }
            ScrollGesturePhase::Cancelled => {
                if state.scroll_gesture_active {
                    Self::post_scroll(state, 0.0, 0.0, K_CG_SCROLL_PHASE_CANCELLED, 0)?;
                }
                state.scroll_gesture_active = false;
                state.scroll_phase_began_pending = false;
            }
            ScrollGesturePhase::MomentumBegan => {
                state.scroll_momentum_active = true;
                state.momentum_phase_began_pending = true;
            }
            ScrollGesturePhase::MomentumEnded => {
                if state.scroll_momentum_active {
                    Self::post_scroll(state, 0.0, 0.0, 0, K_CG_MOMENTUM_SCROLL_PHASE_END)?;
                }
                state.scroll_momentum_active = false;
                state.momentum_phase_began_pending = false;
            }
        }
        Ok(())
    }

    fn set_key(state: &mut InputState, hid_usage: u16, down: bool) -> Result<()> {
        let keycode = hid_to_macos_keycode(hid_usage).ok_or_else(|| {
            Error::InputControl(format!("unsupported HID usage: 0x{hid_usage:02X}"))
        })?;
        unsafe {
            let event = CGEventCreateKeyboardEvent(state.event_source(), keycode, down);
            if event.is_null() {
                return Err(Error::InputControl(
                    "CGEventCreateKeyboardEvent returned null".into(),
                ));
            }
            post_synthetic_event(event);
        }
        if down {
            state.pressed_keys.insert(hid_usage);
        } else {
            state.pressed_keys.remove(&hid_usage);
        }
        Ok(())
    }

    fn commit_text(state: &InputState, text: &str) -> Result<()> {
        // Keep chunks small because some applications ignore very large
        // Unicode payloads attached to a single keyboard event.
        let chars: Vec<char> = text.chars().collect();
        for chunk in chars.chunks(16) {
            let utf16: Vec<u16> = chunk.iter().collect::<String>().encode_utf16().collect();
            unsafe {
                let key_down = CGEventCreateKeyboardEvent(state.event_source(), 0, true);
                if key_down.is_null() {
                    return Err(Error::InputControl(
                        "CGEventCreateKeyboardEvent returned null".into(),
                    ));
                }
                CGEventKeyboardSetUnicodeString(key_down, utf16.len(), utf16.as_ptr());
                post_synthetic_event(key_down);

                let key_up = CGEventCreateKeyboardEvent(state.event_source(), 0, false);
                if key_up.is_null() {
                    return Err(Error::InputControl(
                        "CGEventCreateKeyboardEvent returned null".into(),
                    ));
                }
                post_synthetic_event(key_up);
            }
        }
        Ok(())
    }

    fn release_state(state: &mut InputState) -> Result<()> {
        if let Some(cancel) = state.system_gesture.cancel() {
            let _ = macos_system_gesture::post(cancel);
        }
        if state.scroll_gesture_active {
            let _ = Self::post_scroll(state, 0.0, 0.0, K_CG_SCROLL_PHASE_CANCELLED, 0);
        }
        if state.scroll_momentum_active {
            let _ = Self::post_scroll(state, 0.0, 0.0, 0, K_CG_MOMENTUM_SCROLL_PHASE_END);
        }
        let keys: Vec<u16> = state.pressed_keys.iter().copied().collect();
        for key in keys {
            let _ = Self::set_key(state, key, false);
        }
        let buttons: Vec<MouseButton> = state.pressed_buttons.iter().copied().collect();
        for button in buttons {
            let _ = Self::set_button(state, button, false, 1);
        }
        state.pressed_keys.clear();
        state.pressed_buttons.clear();
        state.scroll_gesture_active = false;
        state.scroll_phase_began_pending = false;
        state.scroll_momentum_active = false;
        state.momentum_phase_began_pending = false;
        Ok(())
    }

    fn validate_events_sync(events: &[InputEvent]) -> Result<()> {
        for event in events {
            match event {
                InputEvent::SystemGesture(gesture) => {
                    if !macos_system_gesture::supported() {
                        return Err(Error::NotSupported(
                            "DockSwipe v1 on this macOS version".into(),
                        ));
                    }
                    gesture
                        .validate()
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
                InputEvent::Key { hid_usage, .. } if hid_to_macos_keycode(*hid_usage).is_none() => {
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

    fn permission_granted(&self, force_refresh: bool) -> Result<bool> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::InputControl("input state lock poisoned".into()))?;
        if !force_refresh
            && state
                .permission_checked_at
                .is_some_and(|checked_at| checked_at.elapsed() < PERMISSION_RECHECK_INTERVAL)
        {
            return Ok(state.permission_granted);
        }
        let granted = unsafe { AXIsProcessTrusted() } != 0;
        state.permission_granted = granted;
        state.permission_checked_at = Some(Instant::now());
        Ok(granted)
    }

    fn enqueue_events(&self, events: &[InputEvent]) -> Result<()> {
        Self::validate_events_sync(events)?;
        if events.is_empty() {
            return Ok(());
        }
        if !self.permission_granted(false)? {
            return Err(Error::InputControl(
                "macOS Accessibility permission is required".into(),
            ));
        }
        // Backpressure the network input stream on the actual serial Quartz
        // application queue. Previously this returned immediately and let
        // tiny motion events accumulate in a second unbounded queue; the
        // cursor then followed old samples after the finger had moved on.
        // SubnetDesk applies its FIFO input synchronously in receive order.
        self.input_queue
            .exec_sync(|| Self::apply_validated_events_sync(self.state.as_ref(), events))
    }

    fn apply_validated_events_sync(state: &Mutex<InputState>, events: &[InputEvent]) -> Result<()> {
        let mut state = state
            .lock()
            .map_err(|_| Error::InputControl("input state lock poisoned".into()))?;
        let permission_stale = state
            .permission_checked_at
            .is_none_or(|checked_at| checked_at.elapsed() >= PERMISSION_RECHECK_INTERVAL);
        if permission_stale {
            state.permission_granted = unsafe { AXIsProcessTrusted() } != 0;
            state.permission_checked_at = Some(Instant::now());
        }
        if !state.permission_granted {
            let _ = Self::release_state(&mut state);
            return Err(Error::InputControl(
                "macOS Accessibility permission is required".into(),
            ));
        }
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
                InputEvent::ScrollGesture { phase } => {
                    Self::set_scroll_gesture_phase(&mut state, *phase)?
                }
                InputEvent::SystemGesture(gesture) => {
                    let phases = state
                        .system_gesture
                        .apply(*gesture)
                        .map_err(|error| Error::InputControl(error.into()))?;
                    for phase in phases {
                        if let Err(error) = macos_system_gesture::post(phase) {
                            if let Some(cancel) = state.system_gesture.cancel() {
                                let _ = macos_system_gesture::post(cancel);
                            }
                            return Err(error);
                        }
                    }
                }
                InputEvent::Key {
                    hid_usage, down, ..
                } => Self::set_key(&mut state, *hid_usage, *down)?,
                InputEvent::TextCommit(text) => Self::commit_text(&state, text)?,
                InputEvent::ReleaseAll => Self::release_state(&mut state)?,
            }
        }
        Ok(())
    }

    fn release_all_sync(state: &Mutex<InputState>) -> Result<()> {
        let mut state = state
            .lock()
            .map_err(|_| Error::InputControl("input state lock poisoned".into()))?;
        Self::release_state(&mut state)
    }

    fn paste_clipboard_sync(state: &Mutex<InputState>) -> Result<()> {
        let mut state = state
            .lock()
            .map_err(|_| Error::InputControl("input state lock poisoned".into()))?;
        let permission_stale = state
            .permission_checked_at
            .is_none_or(|checked_at| checked_at.elapsed() >= PERMISSION_RECHECK_INTERVAL);
        if permission_stale {
            state.permission_granted = unsafe { AXIsProcessTrusted() } != 0;
            state.permission_checked_at = Some(Instant::now());
        }
        if !state.permission_granted {
            let _ = Self::release_state(&mut state);
            return Err(Error::InputControl(
                "macOS Accessibility permission is required".into(),
            ));
        }

        const HID_META: u16 = 0xE3;
        const HID_V: u16 = 0x19;

        // Match PasteGroup's proven sequence: hold Command long enough for
        // the focused application to observe the modifier, click V, then
        // always release Command. Sending the whole sequence without this
        // gap is ignored intermittently by some macOS applications.
        Self::set_key(&mut state, HID_META, true)?;
        std::thread::sleep(Duration::from_millis(50));
        let paste_result = Self::set_key(&mut state, HID_V, true)
            .and_then(|_| Self::set_key(&mut state, HID_V, false));
        let release_result = Self::set_key(&mut state, HID_META, false);
        if paste_result.is_err() || release_result.is_err() {
            let _ = Self::release_state(&mut state);
        }
        paste_result.and(release_result)
    }
}

impl Drop for NativeInputControl {
    fn drop(&mut self) {
        let state = Arc::clone(&self.state);
        self.input_queue.exec_async(move || {
            if let Err(error) = Self::release_all_sync(state.as_ref()) {
                tracing::warn!(%error, "failed to release queued macOS input");
            }
        });
    }
}

#[async_trait::async_trait]
impl InputControlRepository for NativeInputControl {
    fn supports_system_gestures(&self) -> bool {
        macos_system_gesture::supported() && self.permission_granted(false).unwrap_or(false)
    }

    fn permission_state(&self) -> InputPermissionState {
        if self.permission_granted(true).unwrap_or(false) {
            InputPermissionState::Granted
        } else {
            InputPermissionState::Denied
        }
    }

    fn open_permission_settings(&self) -> Result<()> {
        let prompt_options = CFDictionary::from_CFType_pairs(&[(
            CFString::new("AXTrustedCheckOptionPrompt"),
            CFBoolean::true_value(),
        )]);
        unsafe {
            AXIsProcessTrustedWithOptions(prompt_options.as_concrete_TypeRef());
        }
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .spawn()
            .map(|_| ())
            .map_err(Error::Io)
    }

    fn validate_events(&self, events: &[InputEvent]) -> Result<()> {
        Self::validate_events_sync(events)
    }

    async fn apply_events(&self, events: &[InputEvent]) -> Result<()> {
        self.enqueue_events(events)
    }

    async fn paste_clipboard(&self, _is_text: bool) -> Result<()> {
        if !self.permission_granted(false)? {
            return Err(Error::InputControl(
                "macOS Accessibility permission is required".into(),
            ));
        }
        self.input_queue
            .exec_sync(|| Self::paste_clipboard_sync(self.state.as_ref()))
    }

    async fn release_all(&self) -> Result<()> {
        self.enqueue_events(&[InputEvent::ReleaseAll])
    }
}

fn hid_to_macos_keycode(usage: u16) -> Option<u16> {
    Some(match usage {
        0x04 => 0x00,
        0x05 => 0x0B,
        0x06 => 0x08,
        0x07 => 0x02,
        0x08 => 0x0E,
        0x09 => 0x03,
        0x0A => 0x05,
        0x0B => 0x04,
        0x0C => 0x22,
        0x0D => 0x26,
        0x0E => 0x28,
        0x0F => 0x25,
        0x10 => 0x2E,
        0x11 => 0x2D,
        0x12 => 0x1F,
        0x13 => 0x23,
        0x14 => 0x0C,
        0x15 => 0x0F,
        0x16 => 0x01,
        0x17 => 0x11,
        0x18 => 0x20,
        0x19 => 0x09,
        0x1A => 0x0D,
        0x1B => 0x07,
        0x1C => 0x10,
        0x1D => 0x06,
        0x1E => 0x12,
        0x1F => 0x13,
        0x20 => 0x14,
        0x21 => 0x15,
        0x22 => 0x17,
        0x23 => 0x16,
        0x24 => 0x1A,
        0x25 => 0x1C,
        0x26 => 0x19,
        0x27 => 0x1D,
        0x28 => 0x24,
        0x29 => 0x35,
        0x2A => 0x33,
        0x2B => 0x30,
        0x2C => 0x31,
        0x2D => 0x1B,
        0x2E => 0x18,
        0x2F => 0x21,
        0x30 => 0x1E,
        0x31 => 0x2A,
        0x33 => 0x29,
        0x34 => 0x27,
        0x35 => 0x32,
        0x36 => 0x2B,
        0x37 => 0x2F,
        0x38 => 0x2C,
        0x39 => 0x39,
        0x3A => 0x7A,
        0x3B => 0x78,
        0x3C => 0x63,
        0x3D => 0x76,
        0x3E => 0x60,
        0x3F => 0x61,
        0x40 => 0x62,
        0x41 => 0x64,
        0x42 => 0x65,
        0x43 => 0x6D,
        0x44 => 0x67,
        0x45 => 0x6F,
        0x49 => 0x72,
        0x4A => 0x73,
        0x4B => 0x74,
        0x4C => 0x75,
        0x4D => 0x77,
        0x4E => 0x79,
        0x4F => 0x7C,
        0x50 => 0x7B,
        0x51 => 0x7D,
        0x52 => 0x7E,
        0xE0 => 0x3B,
        0xE1 => 0x38,
        0xE2 => 0x3A,
        0xE3 => 0x37,
        0xE4 => 0x3E,
        0xE5 => 0x3C,
        0xE6 => 0x3D,
        0xE7 => 0x36,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" {
        fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    }

    #[test]
    fn system_gesture_native_state_constructs_without_tokio_and_rejects_invalid_input() {
        let platform = NativeInputControl::new();
        assert!(!platform.state.lock().unwrap().system_gesture.is_active());
        assert!(platform
            .validate_events(&[InputEvent::SystemGesture(
                crate::domain::input_control::SystemGestureEvent {
                    axis: 4,
                    phase: 1,
                    progress: 0.0,
                    velocity_x: 0.0,
                    velocity_y: 0.0,
                    inverted_from_device: false,
                }
            )])
            .is_err());
    }

    #[test]
    fn preserves_fractional_pointer_coordinates() {
        let next = offset_point(CGPoint { x: 10.25, y: 20.75 }, 0.125, -0.375);

        assert_eq!(next.x, 10.375);
        assert_eq!(next.y, 20.375);
    }

    #[test]
    fn synthetic_input_uses_the_shared_arcrelay_event_tag() {
        unsafe {
            let event = CGEventCreate(std::ptr::null());
            assert!(!event.is_null());
            mark_synthetic_event(event);
            assert_eq!(
                CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA),
                macos_system_gesture::EVENT_TAG
            );
            CFRelease(event);
        }
    }

    #[test]
    fn maps_all_keys_exposed_by_the_mobile_keyboard() {
        let usages = [
            0x04, 0x06, 0x19, 0x1B, 0x1C, 0x1D, 0x28, 0x29, 0x2A, 0x2B, 0x49, 0x4A, 0x4B, 0x4C,
            0x4D, 0x4E, 0x4F, 0x50, 0x51, 0x52, 0xE0, 0xE1, 0xE2, 0xE3,
        ];

        assert!(usages
            .into_iter()
            .all(|usage| hid_to_macos_keycode(usage).is_some()));
        assert!(hid_to_macos_keycode(0).is_none());
    }
}
