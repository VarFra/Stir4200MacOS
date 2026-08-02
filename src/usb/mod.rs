//! USB transport layer (libusb via `rusb`).
//!
//! M1 — Enumeration: open `066F:4200`, claim the interface, and print the full
//! descriptor tree (device, configurations, interfaces, endpoints), flagging
//! whether the endpoints match what the Linux `stir4200.c` driver assumes:
//!   - control endpoint 0  → register read/write (vendor requests)
//!   - bulk OUT, endpoint 1 → transmit  (`usb_sndbulkpipe(dev, 1)`)
//!   - bulk IN,  endpoint 2 → receive   (`usb_rcvbulkpipe(dev, 2)`)

use std::time::Duration;

use rusb::{Context, Device, DeviceHandle, Direction, TransferType, UsbContext};

/// SigmaTel STIr4200 IrDA/USB bridge.
pub const STIR_VID: u16 = 0x066f;
pub const STIR_PID: u16 = 0x4200;

/// Endpoint numbers the Linux driver assumes (to be confirmed on hardware, M1).
const EXPECTED_BULK_OUT_EP: u8 = 1; // address 0x01
const EXPECTED_BULK_IN_EP: u8 = 2; // address 0x82

/// What we learned about a device's endpoints, so higher layers (and the M1
/// report) can reason about it.
#[derive(Debug, Default, Clone)]
pub struct Endpoints {
    pub interface: u8,
    pub setting: u8,
    pub bulk_out: Option<u8>, // full endpoint address (e.g. 0x01)
    pub bulk_in: Option<u8>,  // full endpoint address (e.g. 0x82)
}

/// Locate the STIr4200 (or any vid:pid) on the bus.
pub fn find_device(ctx: &Context, vid: u16, pid: u16) -> rusb::Result<Option<Device<Context>>> {
    for device in ctx.devices()?.iter() {
        let desc = device.device_descriptor()?;
        if desc.vendor_id() == vid && desc.product_id() == pid {
            return Ok(Some(device));
        }
    }
    Ok(None)
}

/// Print the full descriptor tree of `device` and return the discovered
/// bulk endpoints of the first interface/setting.
pub fn describe_device(device: &Device<Context>) -> rusb::Result<Endpoints> {
    let desc = device.device_descriptor()?;

    println!(
        "Device on bus {:03} address {:03}",
        device.bus_number(),
        device.address()
    );
    println!(
        "  idVendor=0x{:04x} idProduct=0x{:04x}",
        desc.vendor_id(),
        desc.product_id()
    );
    let v = desc.device_version();
    println!(
        "  bcdDevice={}.{}.{}  (chip revision — verify register values apply, brief §6)",
        v.major(),
        v.minor(),
        v.sub_minor()
    );
    let u = desc.usb_version();
    println!("  bcdUSB={}.{}.{}", u.major(), u.minor(), u.sub_minor());
    println!(
        "  bDeviceClass=0x{:02x} bDeviceSubClass=0x{:02x} bDeviceProtocol=0x{:02x}",
        desc.class_code(),
        desc.sub_class_code(),
        desc.protocol_code()
    );
    println!("  bNumConfigurations={}", desc.num_configurations());

    // String descriptors are best-effort (need the device opened).
    if let Ok(handle) = device.open() {
        let timeout = Duration::from_millis(200);
        if let Ok(langs) = handle.read_languages(timeout) {
            if let Some(lang) = langs.first().copied() {
                if let Ok(s) = handle.read_manufacturer_string(lang, &desc, timeout) {
                    println!("  iManufacturer=\"{s}\"");
                }
                if let Ok(s) = handle.read_product_string(lang, &desc, timeout) {
                    println!("  iProduct=\"{s}\"");
                }
                if let Ok(s) = handle.read_serial_number_string(lang, &desc, timeout) {
                    println!("  iSerialNumber=\"{s}\"");
                }
            }
        }
    }

    let mut endpoints = Endpoints::default();
    let mut first_iface_seen = false;

    for cfg_idx in 0..desc.num_configurations() {
        let cfg = match device.config_descriptor(cfg_idx) {
            Ok(c) => c,
            Err(e) => {
                warn!("could not read configuration {cfg_idx}: {e}");
                continue;
            }
        };
        println!(
            "  Configuration {}: {} interface(s), max_power={} mA, self_powered={}",
            cfg.number(),
            cfg.num_interfaces(),
            cfg.max_power(),
            cfg.self_powered()
        );

        for interface in cfg.interfaces() {
            for iface in interface.descriptors() {
                println!(
                    "    Interface {} (alt {}): class=0x{:02x} subclass=0x{:02x} protocol=0x{:02x}, {} endpoint(s)",
                    iface.interface_number(),
                    iface.setting_number(),
                    iface.class_code(),
                    iface.sub_class_code(),
                    iface.protocol_code(),
                    iface.num_endpoints()
                );

                let mut ep = Endpoints {
                    interface: iface.interface_number(),
                    setting: iface.setting_number(),
                    ..Default::default()
                };

                for endpoint in iface.endpoint_descriptors() {
                    let dir = match endpoint.direction() {
                        Direction::In => "IN ",
                        Direction::Out => "OUT",
                    };
                    let tt = match endpoint.transfer_type() {
                        TransferType::Control => "control",
                        TransferType::Isochronous => "isochronous",
                        TransferType::Bulk => "bulk",
                        TransferType::Interrupt => "interrupt",
                    };
                    println!(
                        "      Endpoint 0x{:02x}  {dir}  {tt:<11} max_packet={} interval={}",
                        endpoint.address(),
                        endpoint.max_packet_size(),
                        endpoint.interval()
                    );

                    if endpoint.transfer_type() == TransferType::Bulk {
                        match endpoint.direction() {
                            Direction::Out => ep.bulk_out = Some(endpoint.address()),
                            Direction::In => ep.bulk_in = Some(endpoint.address()),
                        }
                    }
                }

                if !first_iface_seen {
                    endpoints = ep;
                    first_iface_seen = true;
                }
            }
        }
    }

    Ok(endpoints)
}

/// Check the discovered endpoints against the Linux driver's assumptions and
/// print a clear verdict (the M1 acceptance criterion).
pub fn report_expectations(ep: &Endpoints) {
    println!();
    println!("Endpoint check (vs Linux stir4200.c assumptions):");

    match ep.bulk_out {
        Some(addr) => {
            let num = addr & 0x0f;
            let ok = num == EXPECTED_BULK_OUT_EP;
            println!(
                "  bulk OUT : 0x{addr:02x} (ep {num}) — {}",
                if ok { "matches expected ep 1" } else { "DIFFERS from expected ep 1" }
            );
        }
        None => println!("  bulk OUT : MISSING — expected an ep 1 bulk OUT"),
    }
    match ep.bulk_in {
        Some(addr) => {
            let num = addr & 0x0f;
            let ok = num == EXPECTED_BULK_IN_EP;
            println!(
                "  bulk IN  : 0x{addr:02x} (ep {num}) — {}",
                if ok { "matches expected ep 2" } else { "DIFFERS from expected ep 2" }
            );
        }
        None => println!("  bulk IN  : MISSING — expected an ep 2 bulk IN"),
    }
    println!("  control  : endpoint 0 (implicit) — used for register read/write");
}

/// Open the device, detach any kernel driver, and claim the interface.
/// Returns the open handle so later milestones can talk to the endpoints.
pub fn open_and_claim(
    device: &Device<Context>,
    interface: u8,
) -> rusb::Result<DeviceHandle<Context>> {
    let handle = device.open()?;

    // On Linux a kernel driver may bind the device; ask libusb to auto-detach
    // it around our claim. macOS does not support this call — treat the error
    // as benign there (there is no IrDA kernel stack to detach anyway).
    match handle.set_auto_detach_kernel_driver(true) {
        Ok(()) => debug!("auto-detach kernel driver enabled"),
        Err(e) => debug!("set_auto_detach_kernel_driver not supported here: {e}"),
    }

    if let Ok(true) = handle.kernel_driver_active(interface) {
        info!("kernel driver active on interface {interface}, detaching");
        handle.detach_kernel_driver(interface)?;
    }

    handle.claim_interface(interface)?;
    info!("claimed interface {interface}");
    Ok(handle)
}

/// M1 entry point: enumerate, describe, claim, and report.
pub fn run_enumeration(vid: u16, pid: u16) -> Result<(), Box<dyn std::error::Error>> {
    info!("looking for USB device {vid:04x}:{pid:04x}");

    let ctx = Context::new().map_err(|e| {
        format!("failed to initialize libusb: {e}. On macOS ensure the process can access USB.")
    })?;

    let device = match find_device(&ctx, vid, pid)? {
        Some(d) => d,
        None => {
            return Err(format!(
                "device {vid:04x}:{pid:04x} not found. Is the STIr4200 dongle plugged in? \
                 (On macOS, check with `system_profiler SPUSBDataType` / `ioreg -p IOUSB`.)"
            )
            .into());
        }
    };

    let endpoints = describe_device(&device)?;
    report_expectations(&endpoints);

    println!();
    match open_and_claim(&device, endpoints.interface) {
        Ok(_handle) => {
            println!(
                "Opened and claimed interface {} — device is ready for M2 (register I/O).",
                endpoints.interface
            );
        }
        Err(e) => {
            println!("Could not claim the interface: {e}");
            println!(
                "  On macOS this usually means another driver holds the device, or the process \
                 lacks permission. See NOTES.md."
            );
            return Err(e.into());
        }
    }

    Ok(())
}
