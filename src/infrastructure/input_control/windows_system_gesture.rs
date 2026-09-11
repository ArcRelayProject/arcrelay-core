//! Portable system gestures translated to Windows Precision Touchpad input.
//! The optional Windows 11 entry point is resolved at runtime. Failure disables
//! this instance; callers retain the idempotent SwitchSpace command fallback.
use arcrelay_input::SystemGestureEvent;

fn contacts(event: SystemGestureEvent) -> Vec<(i32, i32)> {
    let progress = if event.phase == 1 {
        0.0
    } else {
        event.progress.clamp(-1.2, 1.2)
    };
    match event.axis {
        1 => {
            // Positive v1 progress requests the desktop to the right (fingers left).
            let x = (5_000.0 - progress * 3_200.0).round() as i32;
            let count = if matches!(event.finger_count, 3 | 4) {
                event.finger_count
            } else {
                4
            };
            [1_800, 2_600, 3_400, 4_200]
                .into_iter()
                .take(count as usize)
                .map(|y| (x, y))
                .collect()
        }
        2 => {
            // macOS DockSwipe progress uses the legacy injection sign. Reverse
            // it for physical Windows contact motion: source up must remain up.
            let y = (3_000.0 - progress * 1_800.0).round() as i32;
            let count = if matches!(event.finger_count, 3 | 4) {
                event.finger_count
            } else {
                3
            };
            [2_000, 4_000, 6_000, 8_000]
                .into_iter()
                .take(count as usize)
                .map(|x| (x, y))
                .collect()
        }
        3 => {
            // Positive magnification is a spread. Keep the centroid fixed so
            // Windows recognizes zoom rather than a two-finger pan.
            let direction = if event.inverted_from_device {
                -1.0
            } else {
                1.0
            };
            let half_span = (900.0 + direction * progress * 2_400.0).clamp(300.0, 2_500.0);
            let left = (5_000.0 - half_span).round() as i32;
            let right = (5_000.0 + half_span).round() as i32;
            vec![(left, 3_000), (right, 3_000)]
        }
        _ => Vec::new(),
    }
}

#[cfg(target_os = "windows")]
pub use native::WindowsSystemGesture;

#[cfg(target_os = "windows")]
mod native {
    use super::*;
    use crate::error::{Error, Result};
    use arcrelay_input::SystemGestureSequence;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    use windows::core::{s, w};
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::HMONITOR;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::Win32::UI::Controls::{
        DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE, POINTER_FEEDBACK_MODE,
        POINTER_FEEDBACK_NONE, POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
    };
    use windows::Win32::UI::Input::Pointer::{
        InjectSyntheticPointerInput, POINTER_FLAGS, POINTER_FLAG_CANCELED, POINTER_FLAG_CONFIDENCE,
        POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE, POINTER_TOUCH_INFO,
    };
    use windows::Win32::UI::WindowsAndMessaging::{POINTER_INPUT_TYPE, PT_TOUCHPAD};

    #[repr(C)]
    struct CreationParams {
        pointer_type: POINTER_INPUT_TYPE,
        max_count: u32,
        feedback_mode: POINTER_FEEDBACK_MODE,
        monitor: HMONITOR,
        device_width: u32,
        device_height: u32,
        options: u32,
    }
    type Create = unsafe extern "system" fn(*const CreationParams) -> HSYNTHETICPOINTERDEVICE;

    fn creator() -> Option<Create> {
        static CREATE: OnceLock<Option<Create>> = OnceLock::new();
        *CREATE.get_or_init(|| unsafe {
            let module = GetModuleHandleW(w!("user32.dll")).ok()?;
            let symbol = GetProcAddress(module, s!("CreateSyntheticPointerDevice2"))?;
            Some(std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                Create,
            >(symbol))
        })
    }

    struct Device {
        handle: HSYNTHETICPOINTERDEVICE,
        clock: Instant,
        last_time: u32,
    }
    // Session-scoped synthetic pointer handles have no COM apartment affinity.
    // All access, including destruction, is serialized by the owning mutex.
    unsafe impl Send for Device {}

    impl Device {
        fn create() -> Option<Self> {
            let create = creator()?;
            let parameters = CreationParams {
                pointer_type: PT_TOUCHPAD,
                max_count: 4,
                feedback_mode: POINTER_FEEDBACK_NONE,
                monitor: HMONITOR::default(),
                device_width: 10_000,
                device_height: 6_000,
                options: 1 | 2, /* SDCO_PHYSICAL_SIZE | SDCO_TOUCHPAD_GESTURE_ONLY */
            };
            let handle = unsafe { create(&parameters) };
            (!handle.is_invalid()).then(|| Self {
                handle,
                clock: Instant::now(),
                last_time: 0,
            })
        }

        fn post(&mut self, event: SystemGestureEvent) -> Result<()> {
            let flags = match event.phase {
                1 | 2 => {
                    POINTER_FLAG_CONFIDENCE.0 | POINTER_FLAG_INRANGE.0 | POINTER_FLAG_INCONTACT.0
                }
                8 => POINTER_FLAG_CONFIDENCE.0 | POINTER_FLAG_CANCELED.0,
                _ => POINTER_FLAG_CONFIDENCE.0,
            };
            // Use elapsed time so slow drags and fast flings retain their timing.
            // Strictly increasing timestamps also handle begin/change in one batch.
            self.last_time =
                (self.clock.elapsed().as_millis() as u32).max(self.last_time.saturating_add(1));
            let points: Vec<_> = contacts(event)
                .into_iter()
                .enumerate()
                .map(|(index, (x, y))| {
                    let mut info = POINTER_TOUCH_INFO::default();
                    info.pointerInfo.pointerType = PT_TOUCHPAD;
                    info.pointerInfo.pointerId = index as u32 + 1;
                    info.pointerInfo.pointerFlags = POINTER_FLAGS(flags);
                    info.pointerInfo.ptHimetricLocation = POINT { x, y };
                    info.pointerInfo.dwTime = self.last_time;
                    info.pointerInfo.InputData = 0;
                    POINTER_TYPE_INFO {
                        r#type: PT_TOUCHPAD,
                        Anonymous: POINTER_TYPE_INFO_0 { touchInfo: info },
                    }
                })
                .collect();
            unsafe { InjectSyntheticPointerInput(self.handle, &points) }.map_err(|error| {
                Error::InputControl(format!("Windows system gesture injection failed: {error}"))
            })
        }
    }
    impl Drop for Device {
        fn drop(&mut self) {
            unsafe { DestroySyntheticPointerDevice(self.handle) };
        }
    }

    #[derive(Default)]
    struct State {
        device: Option<Device>,
        attempted: bool,
        sequence: SystemGestureSequence,
        updated: Option<Instant>,
    }
    impl State {
        fn prepare(&mut self) -> bool {
            if !self.attempted {
                self.attempted = true;
                self.device = Device::create();
            }
            self.device.is_some()
        }
        fn cancel(&mut self) {
            if let Some(cancel) = self.sequence.cancel() {
                if let Some(device) = self.device.as_mut() {
                    let _ = device.post(cancel);
                }
            }
            self.updated = None;
        }
    }
    impl Drop for State {
        fn drop(&mut self) {
            self.cancel();
        }
    }

    /// Construction does not create native resources or require Tokio. Capability
    /// negotiation prepares the device; maintenance/release never initialize it.
    #[derive(Default)]
    pub struct WindowsSystemGesture {
        state: Mutex<State>,
    }

    impl WindowsSystemGesture {
        pub fn available(&self) -> bool {
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .prepare()
        }
        pub fn apply(&self, event: SystemGestureEvent) -> Result<()> {
            event
                .validate_format(arcrelay_input::MAX_SYSTEM_GESTURE_FORMAT_VERSION)
                .map_err(|error| Error::InputControl(error.into()))?;
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let phases = state
                .sequence
                .apply(event)
                .map_err(|error| Error::InputControl(error.into()))?;
            if phases.is_empty() {
                return Ok(());
            }
            state.updated = state.sequence.is_active().then(Instant::now);
            for phase in phases {
                if let Some(device) = state.device.as_mut() {
                    if let Err(error) = device.post(phase) {
                        // A failed terminal frame also needs explicit cleanup;
                        // the portable sequence has already ended in that case.
                        let _ = device.post(phase.cancelled());
                        // Do not revoke a working keyboard/mouse lease because an
                        // optional gesture API failed. The window page confirms the
                        // desktop and falls back to SwitchSpace if necessary.
                        tracing::warn!(%error, "disabling Windows native gestures; using desktop command fallback");
                        state.cancel();
                        state.device = None;
                        state.attempted = true;
                        break;
                    }
                }
            }
            Ok(())
        }
        pub fn cancel(&self) {
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cancel();
        }
        pub fn maintain(&self) -> bool {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state
                .updated
                .is_some_and(|time| time.elapsed() >= Duration::from_secs(2))
            {
                state.cancel();
                return true;
            }
            false
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn windows_gesture_construct_release_and_maintenance_need_no_runtime_or_device() {
            let gesture = WindowsSystemGesture::default();
            gesture.cancel();
            assert!(!gesture.maintain());
            let state = gesture.state.lock().unwrap();
            assert!(!state.attempted);
            assert!(state.device.is_none());
        }
        #[test]
        fn invalid_windows_gesture_does_not_initialize_or_mutate_native_state() {
            let gesture = WindowsSystemGesture::default();
            let event = SystemGestureEvent {
                axis: 3,
                phase: 1,
                progress: 0.0,
                velocity_x: 0.0,
                velocity_y: 0.0,
                inverted_from_device: false,
                finger_count: 0,
            };
            assert!(gesture.apply(event).is_err());
            assert!(!gesture.state.lock().unwrap().attempted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(axis: u32, progress: f64) -> SystemGestureEvent {
        SystemGestureEvent {
            axis,
            progress,
            phase: 2,
            velocity_x: 0.0,
            velocity_y: 0.0,
            inverted_from_device: false,
            finger_count: 0,
        }
    }
    #[test]
    fn windows_horizontal_swipe_preserves_three_or_four_contacts_without_a_jump() {
        let start = contacts(event(1, 0.0));
        let right = contacts(event(1, 0.5));
        let left = contacts(event(1, -0.5));
        assert_eq!(start.len(), 4);
        for index in 0..4 {
            assert!(right[index].0 < start[index].0);
            assert!(left[index].0 > start[index].0);
            assert_eq!(right[index].1, start[index].1);
        }
        assert_eq!(
            contacts(SystemGestureEvent {
                phase: 1,
                ..event(1, 1.0)
            }),
            start
        );
        let three = contacts(SystemGestureEvent {
            finger_count: 3,
            ..event(1, 0.5)
        });
        assert_eq!(three.len(), 3);
        assert!(three.iter().all(|point| point.0 == right[0].0));
    }

    #[test]
    fn windows_vertical_swipe_keeps_physical_direction_and_contact_count() {
        let start = contacts(event(2, 0.0));
        let up = contacts(event(2, 0.5));
        let down = contacts(event(2, -0.5));
        assert_eq!(start.len(), 3);
        for index in 0..3 {
            assert!(up[index].1 < start[index].1);
            assert!(down[index].1 > start[index].1);
        }
        assert_eq!(
            contacts(SystemGestureEvent {
                finger_count: 4,
                ..event(2, 0.5)
            })
            .len(),
            4
        );
    }

    #[test]
    fn windows_pinch_uses_two_contacts_about_a_fixed_centroid() {
        let start = contacts(event(3, 0.0));
        let spread = contacts(event(3, 0.5));
        let pinch = contacts(event(3, -0.5));
        assert_eq!(start.len(), 2);
        assert!(spread[0].0 < start[0].0 && spread[1].0 > start[1].0);
        assert!(pinch[0].0 > start[0].0 && pinch[1].0 < start[1].0);
        for points in [start, spread, pinch] {
            assert_eq!(points[0].0 + points[1].0, 10_000);
            assert_eq!(points[0].1, points[1].1);
        }
    }
    #[test]
    fn windows_gesture_extremes_stay_inside_physical_touchpad() {
        for axis in [1, 2] {
            for progress in [-4.0, -1.2, 0.0, 1.2, 4.0] {
                let points = contacts(event(axis, progress));
                assert_eq!(points.len(), if axis == 1 { 4 } else { 3 });
                assert!(points
                    .iter()
                    .all(|(x, y)| (1..10_000).contains(x) && (1..6_000).contains(y)));
            }
        }
        for progress in [-4.0, -1.2, 0.0, 1.2, 4.0] {
            assert!(contacts(event(3, progress))
                .iter()
                .all(|(x, y)| (1..10_000).contains(x) && (1..6_000).contains(y)));
        }
    }
}
