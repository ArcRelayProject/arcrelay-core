//! Bounded legacy DockSwipe injection shared by desktop and mobile controllers.
//! Callers own permissions, lease validation and sequence lifecycle.
use crate::error::{Error, Result};
use arcrelay_input::SystemGestureEvent;
use std::ffi::{c_void, CStr};
use std::sync::OnceLock;

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
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
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
        result == 0
            && CStr::from_bytes_until_nul(&value)
                .ok()
                .and_then(|text| text.to_str().ok())
                .and_then(|text| text.split('.').next()?.parse::<u32>().ok())
                .is_some_and(|major| (12..=26).contains(&major))
    })
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

fn make_phase(event: SystemGestureEvent) -> Result<(OwnedEvent, OwnedEvent)> {
    event
        .validate()
        .map_err(|error| Error::InputControl(error.into()))?;
    let dock = OwnedEvent::new()?;
    let companion = OwnedEvent::new()?;
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
        CGEventSetIntegerValueField(companion.0, 55, 29);
        CGEventSetIntegerValueField(dock.0, FIELD_EVENT_SOURCE_USER_DATA, EVENT_TAG);
        CGEventSetIntegerValueField(companion.0, FIELD_EVENT_SOURCE_USER_DATA, EVENT_TAG);
    }
    Ok((dock, companion))
}

pub fn post(event: SystemGestureEvent) -> Result<()> {
    let (dock, companion) = make_phase(event)?;
    unsafe {
        CGEventPost(1, dock.0);
        CGEventPost(1, companion.0);
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
        };
        let (dock, companion) = make_phase(sample).unwrap();
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
                    };
                    let (dock, companion) = make_phase(sample).unwrap();
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
}
