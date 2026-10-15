//! macOS 27's typed HID payload, attached through SkyLight's setter.
//! References: Mac Mouse Fix PR #1936 (1640bab), TouchSimulator.m, and
//! https://github.com/timmyagentic/mac-mouse-fix-macos-27-fix .
//! No CGEvent memory offsets or caller-supplied native bytes are used.

use super::*;
use objc2::msg_send;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::sel;

const DOCK_SWIPE: u32 = 23;
const VELOCITY: u32 = 9;
const MOTION: u32 = (DOCK_SWIPE << 16) | 1;
const PROGRESS: u32 = (DOCK_SWIPE << 16) | 2;
const FLAVOR: u32 = (DOCK_SWIPE << 16) | 5;
const VELOCITY_X: u32 = VELOCITY << 16;
const VELOCITY_Y: u32 = (VELOCITY << 16) | 1;
const VELOCITY_Z: u32 = (VELOCITY << 16) | 2;
const PHASE_SHIFT: u32 = 24;

type SetHid = unsafe extern "C" fn(CGEventRef, *const AnyObject);
type CopyHid = unsafe extern "C" fn(CGEventRef) -> *mut AnyObject;

struct Bridge {
    class: &'static AnyClass,
    set: SetHid,
    copy: CopyHid,
}

extern "C" {
    fn CGEventGetTimestamp(event: CGEventRef) -> u64;
}

fn bridge() -> Option<&'static Bridge> {
    static BRIDGE: OnceLock<Option<Bridge>> = OnceLock::new();
    BRIDGE
        .get_or_init(|| unsafe { Bridge::load() }.filter(Bridge::probe))
        .as_ref()
}

pub(super) fn available() -> bool {
    bridge().is_some()
}

impl Bridge {
    unsafe fn load() -> Option<Self> {
        let handle = libc::dlopen(
            c"/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight".as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        );
        if handle.is_null() {
            return None;
        }
        let set = libc::dlsym(handle, c"SLEventSetIOHIDEvent".as_ptr());
        let copy = libc::dlsym(handle, c"SLEventCopyIOHIDEvent".as_ptr());
        let class = AnyClass::get(c"HIDEvent");
        let loaded = class.filter(|class| {
            // Check the private ABI before sending messages; absence must only
            // disable this capability, not break mouse/keyboard input.
            [
                sel!(initWithType:timestamp:senderID:),
                sel!(setOptions:),
                sel!(setIntegerValue:forField:),
                sel!(setDoubleValue:forField:),
                sel!(appendEvent:),
                sel!(type),
                sel!(options),
                sel!(children),
                sel!(integerValueForField:),
                sel!(doubleValueForField:),
            ]
            .into_iter()
            .all(|selector| {
                let responds: Bool = msg_send![*class, instancesRespondToSelector: selector];
                responds.as_bool()
            })
        });
        if set.is_null() || copy.is_null() || loaded.is_none() {
            libc::dlclose(handle);
            return None;
        }
        // Keep the framework mapped for the process lifetime, like the class
        // and function pointers it owns. There is one load, never one per frame.
        Some(Self {
            class: loaded?,
            set: std::mem::transmute::<*mut c_void, SetHid>(set),
            copy: std::mem::transmute::<*mut c_void, CopyHid>(copy),
        })
    }

    fn new_event(&self, kind: u32, timestamp: u64) -> Result<Retained<AnyObject>> {
        // SAFETY: load checked the private initializer's known ABI. init
        // consumes alloc's +1; Retained balances it, including failure paths.
        unsafe {
            let allocated: *mut AnyObject = msg_send![self.class, alloc];
            if allocated.is_null() {
                return Err(Error::InputControl("cannot allocate HID gesture".into()));
            }
            let initialized: *mut AnyObject = msg_send![allocated,
                initWithType: kind, timestamp: timestamp, senderID: 0_u64];
            Retained::from_raw(initialized)
                .ok_or_else(|| Error::InputControl("cannot initialize HID gesture".into()))
        }
    }

    fn make_payload(
        &self,
        event: SystemGestureEvent,
        timestamp: u64,
    ) -> Result<Retained<AnyObject>> {
        event
            .validate()
            .map_err(|error| Error::InputControl(error.into()))?;
        let hid = self.new_event(DOCK_SWIPE, timestamp)?;
        let direction = if event.inverted_from_device {
            -1.0
        } else {
            1.0
        };
        // The legacy format pre-flips direction. HID expects device coordinates;
        // progress AND terminal velocities must be normalized to avoid rebound.
        unsafe {
            let () = msg_send![&hid, setOptions: event.phase << PHASE_SHIFT];
            let () = msg_send![&hid, setIntegerValue: event.axis as isize, forField: MOTION];
            let () = msg_send![&hid, setIntegerValue: 3_isize, forField: FLAVOR];
            let () =
                msg_send![&hid, setDoubleValue: event.progress * direction, forField: PROGRESS];
            if matches!(event.phase, 4 | 8) {
                let velocity = self.new_event(VELOCITY, timestamp)?;
                let () = msg_send![&velocity, setDoubleValue: event.velocity_x * direction, forField: VELOCITY_X];
                let () = msg_send![&velocity, setDoubleValue: event.velocity_y * direction, forField: VELOCITY_Y];
                let () = msg_send![&velocity, setDoubleValue: 0.0_f64, forField: VELOCITY_Z];
                let () = msg_send![&hid, appendEvent: Retained::as_ptr(&velocity)];
            }
        }
        Ok(hid)
    }

    fn copy_payload(&self, event: CGEventRef) -> Option<Retained<AnyObject>> {
        // SLEventCopyIOHIDEvent returns an owned Objective-C HIDEvent (+1).
        unsafe { Retained::from_raw((self.copy)(event)) }
    }

    fn probe(&self) -> bool {
        // Do not advertise support based on symbol presence alone. Round-trip
        // all motions through the OS bridge without posting desktop input.
        autoreleasepool(|_| {
            (1..=3).all(|axis| {
                let sample = SystemGestureEvent {
                    axis,
                    phase: 4,
                    progress: -0.25,
                    velocity_x: -3.5,
                    velocity_y: 2.25,
                    inverted_from_device: true,
                    finger_count: 0,
                };
                let Ok((carrier, _)) = make_legacy_phase(sample) else {
                    return false;
                };
                let timestamp = unsafe { CGEventGetTimestamp(carrier.0) };
                let Ok(payload) = self.make_payload(sample, timestamp) else {
                    return false;
                };
                unsafe {
                    (self.set)(carrier.0, Retained::as_ptr(&payload));
                }
                // Verify the attached copy survives release of the input.
                drop(payload);
                let Some(copied) = self.copy_payload(carrier.0) else {
                    return false;
                };
                unsafe {
                    let kind: u32 = msg_send![&copied, type];
                    let options: u32 = msg_send![&copied, options];
                    let motion: isize = msg_send![&copied, integerValueForField: MOTION];
                    let progress: f64 = msg_send![&copied, doubleValueForField: PROGRESS];
                    kind == DOCK_SWIPE
                        && options >> PHASE_SHIFT == 4
                        && motion == axis as isize
                        && (progress - 0.25).abs() < 0.0001
                }
            })
        })
    }
}

pub(super) fn attach(carrier: CGEventRef, event: SystemGestureEvent) -> Result<()> {
    let bridge = bridge()
        .ok_or_else(|| Error::InputControl("macOS HID gesture bridge is unavailable".into()))?;
    autoreleasepool(|_| {
        let payload = bridge.make_payload(event, unsafe { CGEventGetTimestamp(carrier) })?;
        // The setter copies the payload; an extra CFRetain here would leak.
        unsafe {
            (bridge.set)(carrier, Retained::as_ptr(&payload));
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" {
        fn CGEventCreateData(allocator: *const c_void, event: CGEventRef) -> *const c_void;
        fn CGEventCreateFromData(allocator: *const c_void, data: *const c_void) -> CGEventRef;
        fn CFGetRetainCount(object: *const c_void) -> isize;
    }

    #[test]
    fn native_hid_round_trip_preserves_all_axes_phases_directions_and_ownership_without_posting() {
        // Older systems must still run the legacy layout tests. Native HID
        // availability is mandatory on the macOS 27 validation host.
        if os_major() != Some(27) {
            return;
        }
        assert_eq!(
            backend(),
            Some(Backend::Hid),
            "macOS 27 requires a working native HID bridge"
        );
        let bridge = bridge().unwrap();
        autoreleasepool(|_| {
            for axis in 1..=3 {
                for phase in [1, 2, 4, 8] {
                    for inverted in [false, true] {
                        for sign in [-1.0, 1.0] {
                            let sample = SystemGestureEvent {
                                axis,
                                phase,
                                progress: sign * 0.375,
                                velocity_x: sign * 4.5,
                                velocity_y: -sign * 2.25,
                                inverted_from_device: inverted,
                                finger_count: 0,
                            };
                            let (carrier, _) = make_legacy_phase(sample).unwrap();
                            let timestamp = unsafe { CGEventGetTimestamp(carrier.0) };
                            let payload = bridge.make_payload(sample, timestamp).unwrap();
                            let input = Retained::as_ptr(&payload);
                            unsafe {
                                let retains = CFGetRetainCount(input.cast());
                                (bridge.set)(carrier.0, input);
                                assert_eq!(CFGetRetainCount(input.cast()), retains);
                                let attached = bridge.copy_payload(carrier.0).unwrap();
                                assert_ne!(Retained::as_ptr(&attached), input);
                            }
                            drop(payload);
                            // Exercise the serialized CGEvent too: this is what
                            // the event service will transport to WindowServer.
                            let data = unsafe { CGEventCreateData(std::ptr::null(), carrier.0) };
                            assert!(!data.is_null());
                            let restored = OwnedEvent(unsafe {
                                CGEventCreateFromData(std::ptr::null(), data)
                            });
                            unsafe {
                                CFRelease(data.cast_mut());
                            }
                            assert!(!restored.0.is_null());
                            let copied = bridge.copy_payload(restored.0).unwrap();
                            let direction = if inverted { -1.0 } else { 1.0 };
                            unsafe {
                                let kind: u32 = msg_send![&copied, type];
                                let options: u32 = msg_send![&copied, options];
                                let motion: isize =
                                    msg_send![&copied, integerValueForField: MOTION];
                                let flavor: isize =
                                    msg_send![&copied, integerValueForField: FLAVOR];
                                let progress: f64 =
                                    msg_send![&copied, doubleValueForField: PROGRESS];
                                assert_eq!(
                                    (kind, options >> PHASE_SHIFT, motion, flavor),
                                    (23, phase, axis as isize, 3)
                                );
                                assert!((progress - sample.progress * direction).abs() < 0.0001);
                                let children: *mut AnyObject = msg_send![&copied, children];
                                let count: usize = if children.is_null() {
                                    0
                                } else {
                                    msg_send![children, count]
                                };
                                assert_eq!(count, usize::from(matches!(phase, 4 | 8)));
                                if count != 0 {
                                    let child: *mut AnyObject =
                                        msg_send![children, objectAtIndex: 0_usize];
                                    let kind: u32 = msg_send![child, type];
                                    let x: f64 = msg_send![child, doubleValueForField: VELOCITY_X];
                                    let y: f64 = msg_send![child, doubleValueForField: VELOCITY_Y];
                                    assert_eq!(kind, VELOCITY);
                                    assert!((x - sample.velocity_x * direction).abs() < 0.0001);
                                    assert!((y - sample.velocity_y * direction).abs() < 0.0001);
                                }
                            }
                        }
                    }
                }
            }
        });
    }
}
