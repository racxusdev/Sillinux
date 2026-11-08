//! BlueZ over D-Bus (`zbus`, blocking) — scan, pair and connect to the cutter
//! without the external `bluetoothctl` binary.
//!
//! Every function here is meant to run on a worker thread (the D-Bus calls
//! block); the GUI collects results through the same `mpsc` pattern the old
//! scan used. Callers fall back to `bluetoothctl` when the system bus has no
//! BlueZ service.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedObjectPath;

use crate::BtDevice;

type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, zbus::zvariant::OwnedValue>>>;

fn system_bluez() -> Result<Connection, String> {
    let conn = Connection::system().map_err(|e| format!("D-Bus de sistema: {e}"))?;
    // Fail fast when bluetoothd is not running: one cheap call on /org/bluez.
    object_manager(&conn)
        .map(|_| ())
        .map_err(|e| format!("BlueZ indisponível: {e}"))?;
    Ok(conn)
}

fn object_manager(conn: &Connection) -> Result<ManagedObjects, String> {
    let om = Proxy::new(
        conn,
        "org.bluez",
        "/org/bluez",
        "org.freedesktop.DBus.ObjectManager",
    )
    .map_err(|e| e.to_string())?;
    om.call("GetManagedObjects", &())
        .map_err(|e| e.to_string())
}

fn adapter_path(objects: &ManagedObjects) -> Result<OwnedObjectPath, String> {
    objects
        .iter()
        .find(|(_, ifaces)| ifaces.contains_key("org.bluez.Adapter1"))
        .map(|(path, _)| path.clone())
        .ok_or_else(|| "sem adaptador Bluetooth no sistema".to_string())
}

/// Read a string property out of a BlueZ D-Bus value.
fn value_str(value: &zbus::zvariant::OwnedValue) -> Option<String> {
    let value: &zbus::zvariant::Value<'_> = &*value;
    <&str>::try_from(value).ok().map(|s| s.to_string())
}

fn device_path(objects: &ManagedObjects, mac: &str) -> Result<OwnedObjectPath, String> {
    objects
        .iter()
        .find(|(_, ifaces)| {
            ifaces
                .get("org.bluez.Device1")
                .and_then(|props| props.get("Address"))
                .and_then(value_str)
                .map(|a| a.eq_ignore_ascii_case(mac))
                .unwrap_or(false)
        })
        .map(|(path, _)| path.clone())
        .ok_or_else(|| format!("dispositivo {mac} não está visível para o BlueZ"))
}

fn device_proxy<'a>(conn: &'a Connection, path: &OwnedObjectPath) -> Result<Proxy<'a>, String> {
    Proxy::new(conn, "org.bluez", path.clone(), "org.bluez.Device1").map_err(|e| e.to_string())
}

/// Collect every visible BlueZ device (`Address` + `Alias`), deduplicated.
fn collect_devices(objects: &ManagedObjects) -> Vec<BtDevice> {
    let mut out: Vec<BtDevice> = Vec::new();
    for (_, ifaces) in objects {
        let Some(dev) = ifaces.get("org.bluez.Device1") else {
            continue;
        };
        let Some(mac) = dev.get("Address").and_then(value_str) else {
            continue;
        };
        let name = dev.get("Alias").and_then(value_str).unwrap_or_default();
        if let Some(existing) = out.iter_mut().find(|d| d.mac.eq_ignore_ascii_case(&mac)) {
            if existing.name.is_empty() && !name.is_empty() {
                existing.name = name;
            }
        } else {
            out.push(BtDevice {
                mac: mac.to_string(),
                name,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.mac.cmp(&b.mac)));
    out
}

/// Scan for `secs` seconds on the default adapter and list what appeared.
pub fn scan(secs: u64) -> Result<Vec<BtDevice>, String> {
    let conn = system_bluez()?;
    let objects = object_manager(&conn)?;
    let adapter = adapter_path(&objects)?;
    let ap = Proxy::new(&conn, "org.bluez", adapter, "org.bluez.Adapter1")
        .map_err(|e| e.to_string())?;
    ap.call::<_, _, ()>("StartDiscovery", &())
        .map_err(|e| format!("StartDiscovery: {e}"))?;
    let mut devices = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(700));
        devices = collect_devices(&object_manager(&conn)?);
    }
    let _ = ap.call::<_, _, ()>("StopDiscovery", &());
    Ok(devices)
}

/// Pair (if needed), trust and connect a device by MAC address.
pub fn pair_connect(mac: &str) -> Result<(), String> {
    let conn = system_bluez()?;
    let path = device_path(&object_manager(&conn)?, mac)?;
    let dev = device_proxy(&conn, &path)?;

    let paired: bool = dev.get_property("Paired").unwrap_or(false);
    if !paired {
        dev.call::<_, _, ()>("Pair", &())
            .map_err(|e| format!("pair: {e}"))?;
    }
    dev.set_property("Trusted", true)
        .map_err(|e| format!("trust: {e}"))?;
    dev.call::<_, _, ()>("Connect", &())
        .map_err(|e| format!("connect: {e}"))?;
    Ok(())
}
