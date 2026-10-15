//! Explicit native smoke test; default invocation never posts input.
//! cargo run -p arcrelay-core --features native --example macos_system_gesture_probe
//! Add --post AXIS PROGRESS [cancel] to test one gesture on the current desktop.

#[cfg(all(target_os = "macos", feature = "native"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use arcrelay_core::infrastructure::input_control::macos_system_gesture as gesture;
    use arcrelay_input::SystemGestureEvent;
    use std::{thread, time::Duration};

    println!("gesture_backend={}", gesture::backend_name());
    if !gesture::supported() {
        return Err("native system gesture backend is unavailable".into());
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        println!("Native bridge probe passed; no desktop input posted.");
        return Ok(());
    }
    if !(args.len() == 3 || args.len() == 4)
        || args[0] != "--post"
        || (args.len() == 4 && args[3] != "cancel")
    {
        return Err(
            "usage: [--post AXIS PROGRESS [cancel]]; axes: 1 horizontal, 2 vertical, 3 pinch"
                .into(),
        );
    }
    let axis = args[1].parse::<u32>()?;
    let target = args[2].parse::<f64>()?;
    if !target.is_finite() || target == 0.0 || target.abs() > 1.0 {
        return Err("progress must be nonzero and within [-1, 1]".into());
    }
    let cancelled = args.len() == 4;
    let mut event = SystemGestureEvent {
        axis,
        phase: 1,
        progress: 0.0,
        velocity_x: 0.0,
        velocity_y: 0.0,
        inverted_from_device: false,
        finger_count: if axis == 3 { 0 } else { 3 },
    }
    .validate()?;
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }
    if unsafe { AXIsProcessTrusted() } == 0 {
        return Err("Accessibility permission is required; no input was posted".into());
    }
    // Ensure errors also terminate a partially delivered native animation.
    struct Active {
        event: Option<SystemGestureEvent>,
        injector: gesture::Injector,
    }
    impl Drop for Active {
        fn drop(&mut self) {
            if let Some(event) = self.event {
                let _ = self.injector.post(event.cancelled());
            }
        }
    }
    let mut active = Active {
        event: Some(event),
        injector: gesture::Injector::default(),
    };
    active.injector.post(event)?;
    for step in 1..=40 {
        thread::sleep(Duration::from_millis(15));
        event.phase = 2;
        event.progress = target * f64::from(step) / 40.0;
        active.event = Some(event);
        active.injector.post(event)?;
    }
    event.phase = if cancelled { 8 } else { 4 };
    event.velocity_x = if cancelled { 0.0 } else { target * 2.5 };
    event.velocity_y = event.velocity_x;
    active.injector.post(event)?;
    active.event = None;
    println!(
        "Posted axis={axis} progress={target} phase={}; verify the system animation and direction.",
        event.phase
    );
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "native")))]
fn main() {
    eprintln!("This probe requires macOS and --features native.");
}
