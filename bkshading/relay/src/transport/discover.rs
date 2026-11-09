//! Finding the camera on its USB-C network link (shading#6).
//!
//! A Blackmagic camera with Camera OS 9.x enumerates a CDC-NCM network interface next to PTP; the
//! kernel's `cdc_ncm` driver names it `enx<mac>`. camera-box installs a systemd-networkd `.network`
//! for `Driver=cdc_ncm` (DHCPv4, no routes, no DNS; `docs/CONTRACT.md`), and the camera's DHCP
//! server hands the box one address of a /30 — the camera itself is the DHCP server (cam5:
//! camera `.161`, box `.162`).
//!
//! So the camera's address is the DHCP `SERVER_ADDRESS` networkd keeps in its lease file
//! `/run/systemd/netif/leases/<ifindex>` (the `ROUTER` when no server is recorded). That file is
//! networkd's own state, not a documented interface, so when it is missing the relay falls back to
//! the other host of the link's own /30 (`ip -4 -o addr show dev <iface>`). The interface is found
//! by its DRIVER plus the Blackmagic USB vendor id, never by its name: other USB network adapters
//! (2.5G dongles, phone tethering) use `cdc_ncm` too. Discovery reads plain files, so tests point
//! [`DiscoverPaths`] at a fake tree. The caller confirms each address with `GET system/product`.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

/// The kernel driver of the camera's USB network function.
pub const CAMERA_LINK_DRIVER: &str = "cdc_ncm";

/// Blackmagic Design's USB vendor id (`idVendor`, read live on cam5's CDC-NCM function).
pub const BLACKMAGIC_USB_VENDOR: &str = "1edb";

/// Where discovery reads: the sysfs network class and networkd's lease directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverPaths {
    pub sysfs_net: PathBuf,
    pub leases_dir: PathBuf,
    /// The `ip` binary for the /30 fallback.
    pub ip_cmd: PathBuf,
}

impl Default for DiscoverPaths {
    fn default() -> Self {
        DiscoverPaths {
            sysfs_net: PathBuf::from("/sys/class/net"),
            leases_dir: PathBuf::from("/run/systemd/netif/leases"),
            ip_cmd: PathBuf::from("ip"),
        }
    }
}

/// One camera link and what discovery found on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraLink {
    pub iface: String,
    pub ifindex: Option<u32>,
    /// The camera's address, or `None` (no lease and no /30 address yet: the link is down or
    /// DHCP has not finished).
    pub camera_ip: Option<Ipv4Addr>,
    /// Where `camera_ip` came from: `"lease"` or `"/30 peer"`.
    pub source: Option<&'static str>,
}

/// The camera address in a networkd lease file: `SERVER_ADDRESS=`, else the first `ROUTER=`.
pub fn parse_lease(text: &str) -> Option<Ipv4Addr> {
    let value = |key: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key)
                .then(|| v.split_whitespace().next()?.parse::<Ipv4Addr>().ok())
                .flatten()
        })
    };
    value("SERVER_ADDRESS").or_else(|| value("ROUTER"))
}

fn driver_of(iface_dir: &Path) -> Option<String> {
    let target = std::fs::read_link(iface_dir.join("device/driver")).ok()?;
    target.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// The USB vendor id of the device behind an interface: `device` is the USB interface, its parent
/// the USB device carrying `idVendor`.
fn usb_vendor_of(iface_dir: &Path) -> Option<String> {
    std::fs::read_to_string(iface_dir.join("device/../idVendor"))
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
}

/// The first `inet a.b.c.d/N` in `ip -4 -o addr show dev <iface>` output.
pub fn parse_inet(output: &str) -> Option<(Ipv4Addr, u8)> {
    output.lines().find_map(|l| {
        let mut words = l.split_whitespace();
        words.find(|w| *w == "inet")?;
        let (addr, prefix) = words.next()?.split_once('/')?;
        Some((addr.parse().ok()?, prefix.parse().ok()?))
    })
}

/// The other host of a point-to-point subnet: the second usable host of a /30, the other address of
/// a /31. `None` for any other prefix, or the /30's network or broadcast address.
pub fn peer_of(addr: Ipv4Addr, prefix: u8) -> Option<Ipv4Addr> {
    let a = u32::from(addr);
    match prefix {
        30 => match a & 3 {
            1 => Some(Ipv4Addr::from(a + 1)),
            2 => Some(Ipv4Addr::from(a - 1)),
            _ => None,
        },
        31 => Some(Ipv4Addr::from(a ^ 1)),
        _ => None,
    }
}

/// The /30 fallback: the box's own address on `iface`, read with `ip`, and its peer.
fn slash30_peer(ip_cmd: &Path, iface: &str) -> Option<Ipv4Addr> {
    let out = std::process::Command::new(ip_cmd)
        .args(["-4", "-o", "addr", "show", "dev", iface])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let (addr, prefix) = parse_inet(&String::from_utf8_lossy(&out.stdout))?;
    peer_of(addr, prefix)
}

/// Every network interface bound to [`CAMERA_LINK_DRIVER`] on a Blackmagic USB device, sorted by
/// name, with the camera address from its lease, else its /30 peer.
pub fn camera_links(paths: &DiscoverPaths) -> Vec<CameraLink> {
    let Ok(entries) = std::fs::read_dir(&paths.sysfs_net) else {
        return Vec::new();
    };
    let mut links: Vec<CameraLink> = entries
        .flatten()
        .filter_map(|e| {
            let iface = e.file_name().to_string_lossy().into_owned();
            let dir = paths.sysfs_net.join(&iface);
            if driver_of(&dir).as_deref() != Some(CAMERA_LINK_DRIVER) {
                return None;
            }
            let vendor = usb_vendor_of(&dir);
            if vendor.as_deref() != Some(BLACKMAGIC_USB_VENDOR) {
                tracing::debug!(%iface, ?vendor, "cdc_ncm interface skipped: not a Blackmagic USB device");
                return None;
            }
            let ifindex = std::fs::read_to_string(dir.join("ifindex"))
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok());
            let leased = ifindex.and_then(|i| {
                std::fs::read_to_string(paths.leases_dir.join(i.to_string()))
                    .ok()
                    .as_deref()
                    .and_then(parse_lease)
            });
            let (camera_ip, source) = match leased {
                Some(ip) => (Some(ip), Some("lease")),
                None => match slash30_peer(&paths.ip_cmd, &iface) {
                    Some(ip) => (Some(ip), Some("/30 peer")),
                    None => (None, None),
                },
            };
            Some(CameraLink {
                iface,
                ifindex,
                camera_ip,
                source,
            })
        })
        .collect();
    links.sort_by(|a, b| a.iface.cmp(&b.iface));
    links
}

/// One line describing a discovery result, for the log (logged when it changes).
pub fn describe(links: &[CameraLink]) -> String {
    if links.is_empty() {
        return format!("no {CAMERA_LINK_DRIVER} interface of a Blackmagic USB device ({BLACKMAGIC_USB_VENDOR}): camera not on USB, or its firmware has no network function");
    }
    links
        .iter()
        .map(|l| match l.camera_ip {
            Some(ip) => format!(
                "{} -> camera {ip} (from the {})",
                l.iface,
                l.source.unwrap_or("?")
            ),
            None => format!(
                "{} (ifindex {}) has no DHCP lease and no /30 address yet",
                l.iface,
                l.ifindex.map_or_else(|| "?".to_string(), |i| i.to_string())
            ),
        })
        .collect::<Vec<_>>()
        .join("; ")
}
