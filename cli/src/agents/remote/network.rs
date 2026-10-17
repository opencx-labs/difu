//! Addresses a phone on the same Wi-Fi can use to reach this computer.
use std::{
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

/// The QR modal polls every second; interfaces rarely change that fast.
static CACHE: Mutex<Option<(Instant, Vec<String>, String)>> = Mutex::new(None);

/// Bonjour name first (it survives DHCP address changes), then IPv4 addresses
/// with Wi-Fi and Ethernet interfaces ahead of VPN and bridge interfaces.
pub fn hosts() -> Vec<String> {
    cached().0
}

/// Human-readable computer name shown on the phone.
pub fn name() -> String {
    cached().1
}

fn cached() -> (Vec<String>, String) {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    match &*cache {
        Some((at, hosts, name)) if at.elapsed() < Duration::from_secs(10) => {
            (hosts.clone(), name.clone())
        }
        _ => {
            let fresh = (discover_hosts(), discover_name());
            *cache = Some((Instant::now(), fresh.0.clone(), fresh.1.clone()));
            fresh
        }
    }
}

fn discover_hosts() -> Vec<String> {
    // Tests listen on loopback only; that is then the one reachable host.
    if let Ok(bind) = std::env::var("DIFU_REMOTE_BIND") {
        return vec![bind];
    }
    let mut hosts = Vec::new();
    if let Some(name) = bonjour_name() {
        hosts.push(format!("{name}.local"));
    }
    let mut addresses: Vec<(u8, String)> = nix::ifaddrs::getifaddrs()
        .into_iter()
        .flatten()
        .filter(|interface| {
            interface
                .flags
                .contains(nix::net::if_::InterfaceFlags::IFF_UP)
                && !interface
                    .flags
                    .contains(nix::net::if_::InterfaceFlags::IFF_LOOPBACK)
        })
        .filter_map(|interface| {
            let address = interface.address?;
            let ip = IpAddr::V4(address.as_sockaddr_in()?.ip());
            usable(&ip).then(|| (rank(&interface.interface_name), ip.to_string()))
        })
        .collect();
    addresses.sort();
    for (_, address) in addresses {
        if !hosts.contains(&address) {
            hosts.push(address);
        }
    }
    hosts
}

fn discover_name() -> String {
    #[cfg(target_os = "macos")]
    if let Some(name) = scutil("ComputerName") {
        return name;
    }
    nix::unistd::gethostname()
        .ok()
        .and_then(|name| name.into_string().ok())
        .map(|name| name.trim_end_matches(".local").to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "difu".into())
}

fn bonjour_name() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        scutil("LocalHostName")
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux resolves `.local` names only with Avahi; IP addresses follow.
        nix::unistd::gethostname()
            .ok()
            .and_then(|name| name.into_string().ok())
            .map(|name| name.split('.').next().unwrap_or_default().to_owned())
            .filter(|name| !name.is_empty())
    }
}

#[cfg(target_os = "macos")]
fn scutil(key: &str) -> Option<String> {
    let output = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", key])
        .output()
        .ok()?;
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !value.is_empty()).then_some(value)
}

fn usable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified(),
        IpAddr::V6(_) => false,
    }
}

fn rank(interface: &str) -> u8 {
    if interface.starts_with("en") || interface.starts_with("eth") || interface.starts_with("wl") {
        0
    } else if interface.starts_with("bridge") || interface.starts_with("utun") {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_addresses_exclude_loopback_and_rank_wifi_first() {
        assert!(!usable(
            &"127.0.0.1".parse().unwrap_or(IpAddr::from([0, 0, 0, 0]))
        ));
        assert!(!usable(&IpAddr::from([169, 254, 1, 2])));
        assert!(usable(&IpAddr::from([192, 168, 1, 2])));
        assert!(rank("en0") < rank("utun3"));
        assert!(!hosts().iter().any(|h| h == "127.0.0.1"));
        assert!(!name().is_empty());
    }
}
