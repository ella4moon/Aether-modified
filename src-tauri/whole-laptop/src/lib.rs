//! Windows TUN capture feeds the existing final-proxy relay. This crate never
//! receives the remote proxy's address or credentials, or opens that hop itself.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{recover, run_helper_if_requested, Session};

pub const INTERFACE_NAME: &str = "AetherWholeLaptop";
pub const HELPER_ARG: &str = "--whole-laptop-helper";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub local_addr: SocketAddr,
    pub engine_path: PathBuf,
    pub sidecar_path: PathBuf,
    pub data_dir: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct Support {
    pub supported: bool,
    pub elevated: bool,
    pub available: bool,
}

pub fn support(sidecar: &Path) -> Support {
    Support {
        supported: cfg!(windows),
        elevated: is_elevated(),
        available: sidecar.is_file(),
    }
}

pub fn validate_mode(local_addr: SocketAddr, final_proxy_enabled: bool) -> Result<(), String> {
    if !final_proxy_enabled {
        return Err("Enable and configure Final proxy before using Whole laptop.".into());
    }
    if !local_addr.ip().is_loopback() || local_addr.port() == 0 {
        return Err(
            "Whole laptop requires a loopback SOCKS address, such as 127.0.0.1:1819.".into(),
        );
    }
    Ok(())
}

pub fn preflight(request: &Request) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("Whole laptop is available on Windows only.".into());
    }
    if !is_elevated() {
        return Err("Exit Aether-GUI from its tray menu, then right-click it and choose Run as administrator to use Whole laptop.".into());
    }
    validate_mode(request.local_addr, true)?;
    if !request.engine_path.is_file() || !request.sidecar_path.is_file() {
        return Err("Whole-laptop components are missing. Install a new Windows build that includes sing-box.".into());
    }
    Ok(())
}

pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        windows::is_elevated()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

// Windows process lookup may return a different case or omit the extended-path
// prefix. Anchor and escape the entire executable path; never exclude a broad
// directory, filename alone, the browser, or all Windows services.
fn engine_path_pattern(path: &Path) -> String {
    let path = path.to_string_lossy();
    let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
    let mut pattern = String::from(r"(?i)^(\\\\\?\\)?");
    for c in path.chars() {
        if r"\.+*?()|[]{}^$".contains(c) {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('$');
    pattern
}

/// sing-box 1.14.1 configuration. An explicit DNS detour is essential: the new
/// DNS server syntax otherwise dials directly. Literal server IP avoids a
/// bootstrap resolver. UDP/ICMP never fall back to the physical connection.
pub fn config(request: &Request) -> Result<Value, String> {
    validate_mode(request.local_addr, true)?;
    Ok(json!({
        "log": {"level": "info", "timestamp": false, "disabled": false},
        "dns": {
            "servers": [{
                "type": "https", "tag": "through-final-proxy", "server": "1.1.1.1",
                "server_port": 443, "path": "/dns-query",
                "tls": {"enabled": true, "server_name": "cloudflare-dns.com"},
                "detour": "local-final-proxy"
            }],
            "final": "through-final-proxy", "strategy": "prefer_ipv4"
        },
        "inbounds": [{
            "type": "tun", "tag": "windows-apps", "interface_name": INTERFACE_NAME,
            "address": ["172.31.255.1/30", "fdfe:ae7e:1819::1/126"],
            "mtu": 1500, "auto_route": true, "strict_route": true,
            "dns_mode": "hijack", "stack": "mixed"
        }],
        "outbounds": [
            {"type": "socks", "tag": "local-final-proxy", "version": "5",
             "server": request.local_addr.ip().to_string(),
             "server_port": request.local_addr.port(), "network": "tcp"},
            {"type": "direct", "tag": "aether-transport"}
        ],
        "route": {
            "auto_detect_interface": true,
            "rules": [
                {"process_path_regex": [engine_path_pattern(&request.engine_path)],
                 "action": "route", "outbound": "aether-transport"},
                {"port": 53, "action": "hijack-dns"},
                {"network": ["udp", "icmp"], "action": "reject"}
            ],
            "final": "local-final-proxy"
        }
    }))
}

#[cfg(windows)]
#[derive(Debug, Deserialize, Serialize)]
enum Event {
    Ready,
    Failed(String),
}

#[cfg(not(windows))]
pub struct Session;
#[cfg(not(windows))]
impl Session {
    pub fn start(_: Request) -> Result<Self, String> {
        Err("Whole laptop requires Windows.".into())
    }
    pub fn poll(&mut self) -> Result<bool, String> {
        Err("Whole laptop requires Windows.".into())
    }
    pub fn stop(&mut self) -> Result<(), String> {
        Ok(())
    }
}
#[cfg(not(windows))]
pub fn run_helper_if_requested() -> Option<i32> {
    None
}
#[cfg(not(windows))]
pub fn recover(_: &Path) -> Result<(), String> {
    Err("Whole laptop requires Windows.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request {
        Request {
            local_addr: "127.0.0.1:1819".parse().unwrap(),
            engine_path: r"C:\Program Files\Aether (test)\binaries\aether.exe".into(),
            sidecar_path: r"C:\Program Files\Aether (test)\binaries\sing-box.exe".into(),
            data_dir: r"C:\Users\test\AppData\Roaming\com.cluvexstudio.aethergui".into(),
        }
    }

    #[test]
    fn whole_laptop_requires_final_proxy_and_loopback() {
        assert!(validate_mode("127.0.0.1:1819".parse().unwrap(), false).is_err());
        for addr in [
            "0.0.0.0:1819",
            "192.168.1.2:1819",
            "[::]:1819",
            "127.0.0.1:0",
        ] {
            assert!(validate_mode(addr.parse().unwrap(), true).is_err());
        }
        assert!(validate_mode("[::1]:1819".parse().unwrap(), true).is_ok());
    }

    #[test]
    fn internet_and_dns_use_the_same_relay_with_no_general_direct_fallback() {
        let c = config(&request()).unwrap();
        assert_eq!(c["route"]["final"], "local-final-proxy");
        assert_eq!(c["dns"]["servers"][0]["detour"], c["route"]["final"]);
        assert_eq!(c["dns"]["servers"][0]["server"], "1.1.1.1");
        let outbounds = c["outbounds"].as_array().unwrap();
        assert_eq!(outbounds.len(), 2);
        assert_eq!(outbounds[0]["server"], "127.0.0.1");
        assert_eq!(outbounds[0]["server_port"], 1819);
        let rules = c["route"]["rules"].as_array().unwrap();
        let direct: Vec<_> = rules
            .iter()
            .filter(|r| r["outbound"] == "aether-transport")
            .collect();
        assert_eq!(direct.len(), 1);
        assert!(direct[0]["process_path_regex"].is_array());
        assert_eq!(rules[1]["port"], 53);
        assert_eq!(rules[1]["action"], "hijack-dns");
        assert_eq!(rules[2]["network"], json!(["udp", "icmp"]));
        assert_eq!(rules[2]["action"], "reject");
    }

    #[test]
    fn both_ip_families_are_captured_and_recursion_is_prevented() {
        let c = config(&request()).unwrap();
        let inbound = &c["inbounds"][0];
        assert_eq!(inbound["auto_route"], true);
        assert_eq!(inbound["strict_route"], true);
        let addresses = inbound["address"].as_array().unwrap();
        assert!(addresses.iter().any(|a| a.as_str().unwrap().contains(':')));
        assert!(addresses.iter().any(|a| a.as_str().unwrap().contains('.')));
        assert_eq!(c["route"]["auto_detect_interface"], true);
    }

    #[test]
    fn only_the_exact_engine_path_is_excluded_regardless_of_case_or_prefix() {
        let path = request().engine_path;
        let regex = regex::Regex::new(&engine_path_pattern(&path)).unwrap();
        assert!(regex.is_match(path.to_str().unwrap()));
        assert!(regex.is_match(r"c:\program files\aether (test)\binaries\AETHER.EXE"));
        assert!(regex.is_match(r"\\?\C:\Program Files\Aether (test)\binaries\aether.exe"));
        assert!(!regex.is_match(r"C:\Downloads\aether.exe"));
        assert!(!regex.is_match(r"C:\Program Files\Aether (test)\binaries\aetherXexe"));
        assert!(!regex.is_match(r"C:\Program Files\Aether (test)\binaries\aether.exe.evil"));
    }
}
