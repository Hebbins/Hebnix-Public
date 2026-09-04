//! diagnostic: raw gilrs view of connected controllers, unfiltered by our
//! own layout/label guessing. shows exactly what gilrs itself reports for
//! name/uuid/vendor/product/mapping_source, and prints every button press
//! as (raw evcode, canonical Button gilrs assigned it).
//!
//! run: cargo run -p hebnix-sdk --example gamepad_probe
//! then press buttons one at a time and read them off here.

use gilrs::{Event, EventType, GilrsBuilder};

fn main() {
    let mut gilrs = GilrsBuilder::new()
        .add_included_mappings(false)
        .add_mappings(include_str!("../assets/gamecontrollerdb.txt"))
        .build()
        .expect("failed to init gilrs");

    println!("connected gamepads:");
    for (id, gamepad) in gilrs.gamepads() {
        println!(
            "  id={id:?} name={:?} os_name={:?} uuid={:?} vendor_id={:?} product_id={:?} mapping_source={:?}",
            gamepad.name(),
            gamepad.os_name(),
            gamepad.uuid(),
            gamepad.vendor_id(),
            gamepad.product_id(),
            gamepad.mapping_source(),
        );
    }
    println!("\npress buttons one at a time (ctrl+c to quit)...\n");

    loop {
        while let Some(Event { id, event, .. }) = gilrs.next_event() {
            match event {
                EventType::ButtonPressed(button, code) => {
                    println!("PRESSED  button={button:?} code={code:?} gamepad={id:?}");
                }
                EventType::ButtonReleased(button, code) => {
                    println!("released button={button:?} code={code:?} gamepad={id:?}");
                }
                EventType::Connected => println!("connected: {id:?}"),
                EventType::Disconnected => println!("disconnected: {id:?}"),
                _ => {}
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}
