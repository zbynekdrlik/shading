//! Finding the camera on its USB-C network link (shading#6).
//!
//! A Blackmagic camera with Camera OS 9.x enumerates a CDC-NCM network interface next to PTP; the
//! kernel's `cdc_ncm` driver names it `enx<mac>`. camera-box installs a systemd-networkd `.network`
//! for `Driver=cdc_ncm` (DHCPv4, no routes, no DNS; `docs/CONTRACT.md`), and the camera's DHCP
//! server hands the box one address of a /30 — the camera itself is the DHCP server (cam5:
//! camera `.161`, box `.162`).
//!
//! So the camera's address is the DHCP `SERVER_ADDRESS` networkd keeps in its lease file
//! `/run/systemd/netif/leases/<ifindex>` (the `ROUTER` when no server is recorded). The interface
//! is found by its DRIVER, never by its name. Everything reads plain files, so tests point
//! [`DiscoverPaths`] at a fake tree. The caller confirms each address with `GET system/product`.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

/// The kernel driver of the camera's USB network function.
pub const CAMERA_LINK_DRIVER: &str = "cdc_ncm";

/// Where discovery reads: the sysfs network class and networkd's lease directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverPaths {
    pub sysfs_net: PathBuf,
    pub leases_dir: PathBuf,
}

impl Default for DiscoverPaths {
    fn default() -> Self {
        DiscoverPaths {
            sysfs_net: PathBuf::from("/sys/class/net"),
            leases_dir: PathBuf::from("/run/systemd/netif/leases"),
        }
    }
}

/// One camera link and what discovery found on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraLink {
    pub iface: String,
    pub ifindex: Option<u32>,
    /// The camera's address from the lease, or `None` (no lease yet: the link is down, DHCP has
    /// not finished, or networkd does not manage it).
    pub camera_ip: Option<Ipv4Addr>,
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

/// Every network interface bound to [`CAMERA_LINK_DRIVER`], sorted by name, with the camera
/// address from its lease.
pub fn camera_links(paths: &DiscoverPaths) -> Vec<CameraLink> {
    let Ok(entries) = std::fs::read_dir(&paths.sysfs_net) else {
        return Vec::new();
    };
    let mut links: Vec<CameraLink> = entries
        .flatten()
        .filter_map(|e| {
            let iface = e.file_name().to_string_lossy().into_owned();
            let dir = paths.sysfs_net.join(&iface);
            (driver_of(&dir).as_deref() == Some(CAMERA_LINK_DRIVER)).then(|| {
                let ifindex = std::fs::read_to_string(dir.join("ifindex"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok());
                let camera_ip = ifindex.and_then(|i| {
                    std::fs::read_to_string(paths.leases_dir.join(i.to_string()))
                        .ok()
                        .as_deref()
                        .and_then(parse_lease)
                });
                CameraLink {
                    iface,
                    ifindex,
                    camera_ip,
                }
            })
        })
        .collect();
    links.sort_by(|a, b| a.iface.cmp(&b.iface));
    links
}

/// One line describing a discovery result, for the log (logged when it changes).
pub fn describe(links: &[CameraLink]) -> String {
    if links.is_empty() {
        return format!("no {CAMERA_LINK_DRIVER} interface (camera not on USB, or its firmware has no network function)");
    }
    links
        .iter()
        .map(|l| match l.camera_ip {
            Some(ip) => format!("{} -> camera {ip}", l.iface),
            None => format!(
                "{} (ifindex {}) has no DHCP lease yet",
                l.iface,
                l.ifindex.map_or_else(|| "?".to_string(), |i| i.to_string())
            ),
        })
        .collect::<Vec<_>>()
        .join("; ")
}
