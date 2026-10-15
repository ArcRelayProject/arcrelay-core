//! Bounded DockSwipe injection shared by desktop and mobile controllers.
//! Callers own permissions, lease validation and sequence lifecycle.
use crate::error::{Error, Result};
use arcrelay_input::SystemGestureEvent;
use std::ffi::{c_void, CStr};
use std::sync::OnceLock;

mod hid;

pub const EVENT_TAG: i64 = 0x4152_4349_4E50_5554;
const FIELD_EVENT_SOURCE_USER_DATA: u32 = 42;
type CGEventRef = *mut c_void;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn CGEventCreate(source: *const c_void) -> CGEventRef;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventSetDoubleValueField(event: CGEventRef, field: u32, value: f64);
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CFRelease(value: *const c_void);
}
pub fn supported() -> bool {
    backend().is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Legacy,
    Hid,
}

fn select_backend(major: u32, hid_ready: bool) -> Option<Backend> {
    match major {
        12..=26 => Some(Backend::Legacy),
        27 if hid_ready => Some(Backend::Hid),
        _ => None,
    }
}

fn backend() -> Option<Backend> {
    static BACKEND: OnceLock<Option<Backend>> = OnceLock::new();
    *BACKEND.get_or_init(|| {
        let major = os_major()?;
        // Future releases stay unadvertised until their native format is verified.
        select_backend(major, major == 27 && hid::available())
    })
}

fn os_major() -> Option<u32> {
    let mut value = [0u8; 128];
    let mut size = value.len();
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            value.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return None;
    }
    CStr::from_bytes_until_nul(&value)
        .ok()?
        .to_str()
        .ok()?
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// Native path selected after the HID bridge has passed a non-posting probe.
pub fn backend_name() -> &'static str {
    match backend() {
        Some(Backend::Legacy) => "legacy-dockswipe",
        Some(Backend::Hid) => "hid-dockswipe",
        None => "unsupported",
    }
}

struct OwnedEvent(CGEventRef);
impl OwnedEvent {
    fn new() -> Result<Self> {
        let event = unsafe { CGEventCreate(std::ptr::null()) };
        if event.is_null() {
            return Err(Error::InputControl("cannot allocate system gesture".into()));
        }
        Ok(Self(event))
    }
}
impl Drop for OwnedEvent {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.0);
        }
    }
}

fn make_legacy_phase(event: SystemGestureEvent) -> Result<(OwnedEvent, OwnedEvent)> {
    let dock = make_dock_phase(event)?;
    let companion = OwnedEvent::new()?;
    unsafe {
        CGEventSetIntegerValueField(companion.0, 55, 29);
        CGEventSetIntegerValueField(companion.0, FIELD_EVENT_SOURCE_USER_DATA, EVENT_TAG);
    }
    Ok((dock, companion))
}

fn make_dock_phase(event: SystemGestureEvent) -> Result<OwnedEvent> {
    event
        .validate()
        .map_err(|error| Error::InputControl(error.into()))?;
    let dock = OwnedEvent::new()?;
    unsafe {
        for (field, value) in [
            (55, 30),
            (110, 23),
            (132, i64::from(event.phase)),
            (134, i64::from(event.phase)),
            (123, i64::from(event.axis)),
            (165, i64::from(event.axis)),
            (135, (event.progress as f32).to_bits() as i64),
            (136, i64::from(event.inverted_from_device)),
        ] {
            CGEventSetIntegerValueField(dock.0, field, value);
        }
        CGEventSetDoubleValueField(dock.0, 124, event.progress);
        let encoded_axis = f32::from_bits(event.axis) as f64;
        CGEventSetDoubleValueField(dock.0, 119, encoded_axis);
        CGEventSetDoubleValueField(dock.0, 139, encoded_axis);
        CGEventSetDoubleValueField(dock.0, 129, event.velocity_x);
        CGEventSetDoubleValueField(dock.0, 130, event.velocity_y);
        CGEventSetIntegerValueField(dock.0, FIELD_EVENT_SOURCE_USER_DATA, EVENT_TAG);
    }
    Ok(dock)
}

/// Per-input-session native state. The caller's SystemGestureSequence still
/// owns ordering, replacement and disconnect cancellation.
#[derive(Debug, Default)]
pub struct Injector {
    pending_begin: Option<SystemGestureEvent>,
    magnification_progress: f64,
}

impl Injector {
    pub fn post(&mut self, event: SystemGestureEvent) -> Result<()> {
        let backend = backend().ok_or_else(|| {
            Error::InputControl("native system gesture backend is unavailable".into())
        })?;
        // v3 identifies two-finger magnification separately from the system
        // pinch. It still uses a type-29 Zoom event on macOS 27, not DockSwipe.
        if event.axis == 3 && event.finger_count == 2 {
            event
                .validate()
                .map_err(|error| Error::InputControl(error.into()))?;
            let previous = if event.phase == 1 {
                0.0
            } else {
                self.magnification_progress
            };
            let native = make_magnification_phase(event, event.progress - previous)?;
            unsafe {
                CGEventPost(0, native.0);
            }
            self.magnification_progress = if matches!(event.phase, 4 | 8) {
                0.0
            } else {
                event.progress
            };
            return Ok(());
        }
        self.submit(event, backend, post_native)
    }

    fn submit(
        &mut self,
        event: SystemGestureEvent,
        backend: Backend,
        mut deliver: impl FnMut(SystemGestureEvent) -> Result<()>,
    ) -> Result<()> {
        event
            .validate()
            .map_err(|error| Error::InputControl(error.into()))?;
        if backend == Backend::Hid {
            if event.phase == 1 {
                self.pending_begin = (event.progress == 0.0).then_some(event);
                if self.pending_begin.is_some() {
                    return Ok(());
                }
            } else if let Some(begin) = self.pending_begin {
                if begin.axis != event.axis
                    || begin.inverted_from_device != event.inverted_from_device
                    || begin.finger_count != event.finger_count
                {
                    return Err(Error::InputControl(
                        "pending native gesture route changed".into(),
                    ));
                }
                if matches!(event.phase, 4 | 8) {
                    // No movement ever reached the OS, so no native animation
                    // exists to end. Do not inject an orphan terminal phase.
                    self.pending_begin = None;
                    return Ok(());
                }
                if event.progress == 0.0 {
                    return Ok(());
                }
                // macOS 27's vertical consumer ignores a zero-progress Began.
                // Seed Began with the first actual position, not a guessed
                // epsilon or synthetic velocity, preserving slow/reversed input.
                deliver(SystemGestureEvent {
                    phase: 1,
                    velocity_x: 0.0,
                    velocity_y: 0.0,
                    ..event
                })?;
                self.pending_begin = None;
                return Ok(());
            }
        } else {
            self.pending_begin = None;
        }
        deliver(event)
    }
}

fn make_magnification_phase(event: SystemGestureEvent, delta: f64) -> Result<OwnedEvent> {
    let native = OwnedEvent::new()?;
    unsafe {
        for (field, value) in [
            (55, 29),
            (110, 8),
            (132, i64::from(event.phase)),
            (FIELD_EVENT_SOURCE_USER_DATA, EVENT_TAG),
        ] {
            CGEventSetIntegerValueField(native.0, field, value);
        }
        CGEventSetDoubleValueField(native.0, 113, delta);
    }
    Ok(native)
}

fn post_native(event: SystemGestureEvent) -> Result<()> {
    let backend = backend().ok_or_else(|| {
        Error::InputControl("native system gesture backend is unavailable".into())
    })?;
    let (dock, companion) = match backend {
        Backend::Legacy => {
            let (dock, companion) = make_legacy_phase(event)?;
            (dock, Some(companion))
        }
        Backend::Hid => {
            let dock = make_dock_phase(event)?;
            hid::attach(dock.0, event)?;
            (dock, None)
        }
    };
    unsafe {
        CGEventPost(1, dock.0);
        // macOS 27 uses the embedded HID event; it needs no legacy marker.
        if let Some(companion) = companion {
            CGEventPost(1, companion.0);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    extern "C" {
        fn CGEventGetType(event: CGEventRef) -> u32;
        fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
        fn CGEventGetDoubleValueField(event: CGEventRef, field: u32) -> f64;
    }

    #[test]
    fn system_gesture_layout_is_shared_and_constructed_without_posting() {
        let sample = SystemGestureEvent {
            axis: 2,
            phase: 1,
            progress: -0.375,
            velocity_x: -4.5,
            velocity_y: -4.5,
            inverted_from_device: false,
            finger_count: 0,
        };
        let (dock, companion) = make_legacy_phase(sample).unwrap();
        unsafe {
            assert_eq!(CGEventGetType(dock.0), 30);
            assert_eq!(CGEventGetType(companion.0), 29);
            for (field, expected) in [(110, 23), (123, 2), (132, 1), (134, 1), (165, 2)] {
                assert_eq!(CGEventGetIntegerValueField(dock.0, field), expected);
            }
            assert_eq!(CGEventGetDoubleValueField(dock.0, 124), sample.progress);
            assert_eq!(CGEventGetDoubleValueField(dock.0, 129), sample.velocity_x);
            assert_eq!(CGEventGetDoubleValueField(dock.0, 130), sample.velocity_y);
            assert_eq!(CGEventGetIntegerValueField(dock.0, 42), EVENT_TAG);
            assert_eq!(CGEventGetIntegerValueField(companion.0, 42), EVENT_TAG);
        }
    }

    #[test]
    fn pinch_and_spread_preserve_native_layout_through_all_phases_without_posting() {
        for inverted_from_device in [false, true] {
            for sign in [-1.0, 1.0] {
                for phase in [1, 2, 4, 8] {
                    let sample = SystemGestureEvent {
                        axis: 3,
                        phase,
                        progress: sign * 0.5,
                        velocity_x: sign * 4.5,
                        velocity_y: sign * 4.5,
                        inverted_from_device,
                        finger_count: 0,
                    };
                    let (dock, companion) = make_legacy_phase(sample).unwrap();
                    unsafe {
                        assert_eq!(CGEventGetType(dock.0), 30);
                        assert_eq!(CGEventGetType(companion.0), 29);
                        for (field, expected) in [
                            (110, 23),
                            (123, 3),
                            (165, 3),
                            (132, phase as i64),
                            (134, phase as i64),
                            (135, (sample.progress as f32).to_bits() as i64),
                            (136, i64::from(inverted_from_device)),
                        ] {
                            assert_eq!(CGEventGetIntegerValueField(dock.0, field), expected);
                        }
                        for (field, expected) in [
                            (124, sample.progress),
                            (129, sample.velocity_x),
                            (130, sample.velocity_y),
                            (119, f32::from_bits(3) as f64),
                            (139, f32::from_bits(3) as f64),
                        ] {
                            assert_eq!(CGEventGetDoubleValueField(dock.0, field), expected);
                        }
                        assert_eq!(CGEventGetIntegerValueField(dock.0, 42), EVENT_TAG);
                        assert_eq!(CGEventGetIntegerValueField(companion.0, 42), EVENT_TAG);
                    }
                }
            }
        }
    }

    #[test]
    fn backend_selection_requires_the_verified_hid_bridge_only_on_27() {
        for major in [12, 15, 26] {
            assert_eq!(select_backend(major, false), Some(Backend::Legacy));
        }
        assert_eq!(select_backend(27, false), None);
        assert_eq!(select_backend(27, true), Some(Backend::Hid));
        for major in [0, 11, 28, u32::MAX] {
            assert_eq!(select_backend(major, true), None);
        }
    }

    #[test]
    fn hid_zero_begin_waits_for_real_motion_and_preserves_three_and_four_finger_sequences() {
        for fingers in [0, 3, 4] {
            for sign in [-1.0, 1.0] {
                let mut injector = Injector::default();
                let mut delivered = Vec::new();
                let mut send = |event| {
                    delivered.push(event);
                    Ok(())
                };
                let begin = SystemGestureEvent {
                    axis: 2,
                    phase: 1,
                    progress: 0.0,
                    velocity_x: 0.0,
                    velocity_y: 0.0,
                    inverted_from_device: false,
                    finger_count: fingers,
                };
                injector.submit(begin, Backend::Hid, &mut send).unwrap();
                injector
                    .submit(
                        SystemGestureEvent { phase: 2, ..begin },
                        Backend::Hid,
                        &mut send,
                    )
                    .unwrap();
                for (phase, progress) in [(2, sign * 0.02), (2, sign * 0.3), (4, sign * 0.4)] {
                    injector
                        .submit(
                            SystemGestureEvent {
                                phase,
                                progress,
                                ..begin
                            },
                            Backend::Hid,
                            &mut send,
                        )
                        .unwrap();
                }
                assert_eq!(
                    delivered
                        .iter()
                        .map(|event| (event.phase, event.progress))
                        .collect::<Vec<_>>(),
                    vec![(1, sign * 0.02), (2, sign * 0.3), (4, sign * 0.4)]
                );
                assert!(delivered.iter().all(|event| event.finger_count == fingers));
                assert!(injector.pending_begin.is_none());
            }
        }
    }

    #[test]
    fn pending_cancel_posts_nothing_and_legacy_zero_begin_is_unchanged() {
        let mut injector = Injector::default();
        let begin = SystemGestureEvent {
            axis: 1,
            phase: 1,
            progress: 0.0,
            velocity_x: 0.0,
            velocity_y: 0.0,
            inverted_from_device: false,
            finger_count: 4,
        };
        let mut delivered = Vec::new();
        for terminal in [4, 8] {
            injector
                .submit(begin, Backend::Hid, |event| {
                    delivered.push(event);
                    Ok(())
                })
                .unwrap();
            injector
                .submit(
                    SystemGestureEvent {
                        phase: terminal,
                        ..begin
                    },
                    Backend::Hid,
                    |event| {
                        delivered.push(event);
                        Ok(())
                    },
                )
                .unwrap();
            assert!(injector.pending_begin.is_none());
        }
        assert!(delivered.is_empty());
        injector
            .submit(begin, Backend::Legacy, |event| {
                delivered.push(event);
                Ok(())
            })
            .unwrap();
        assert_eq!(delivered, vec![begin]);
    }

    #[test]
    fn two_finger_magnification_is_a_zoom_event_instead_of_a_system_pinch() {
        let zoom = SystemGestureEvent {
            axis: 3,
            phase: 2,
            progress: 0.35,
            velocity_x: 0.0,
            velocity_y: 0.0,
            inverted_from_device: false,
            finger_count: 2,
        };
        let native = make_magnification_phase(zoom, zoom.progress - 0.15).unwrap();
        unsafe {
            assert_eq!(CGEventGetType(native.0), 29);
            assert_eq!(CGEventGetIntegerValueField(native.0, 110), 8);
            assert_eq!(CGEventGetIntegerValueField(native.0, 132), 2);
            assert!((CGEventGetDoubleValueField(native.0, 113) - 0.2).abs() < 0.00001);
            assert_eq!(CGEventGetIntegerValueField(native.0, 42), EVENT_TAG);
        }
    }
}
