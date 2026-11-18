//! Background health checks (spec §4.7): CPU temperature, disk usage and
//! Immich server reachability. They run on their own threads and only
//! publish a [`Status`] snapshot, so a slow check can never stall a frame.

use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::Config;

const TEMP_PATH: &str = "/sys/class/thermal/thermal_zone0/temp";
const TEMP_INTERVAL: Duration = Duration::from_secs(1);
const DISK_INTERVAL: Duration = Duration::from_secs(5 * 60);
const NET_INTERVAL_ONLINE: Duration = Duration::from_secs(60);
const NET_INTERVAL_OFFLINE: Duration = Duration::from_secs(10);
const NET_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    #[default]
    Ok,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Network {
    /// No Immich server configured: nothing to check, no icon.
    #[default]
    Disabled,
    Unknown,
    Online,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Status {
    pub temp_c: Option<f32>,
    pub temp: Level,
    pub disk_pct: Option<f32>,
    pub disk: Level,
    pub network: Network,
}

/// v1 thresholds semantics: above `warn` is a warning, `crit` and above is critical.
pub fn classify(value: f32, warn: f32, crit: f32) -> Level {
    if value >= crit {
        Level::Critical
    } else if value > warn {
        Level::Warning
    } else {
        Level::Ok
    }
}

/// Parse the kernel's thermal zone file (millidegrees Celsius).
pub fn parse_millidegrees(text: &str) -> Option<f32> {
    text.trim().parse::<i64>().ok().map(|m| m as f32 / 1000.0)
}

/// Used space as a percentage, computed like v1 (`(blocks - bfree) / blocks`).
pub fn disk_used_pct(path: &Path) -> std::io::Result<f32> {
    let st = nix::sys::statvfs::statvfs(path).map_err(std::io::Error::from)?;
    let blocks = st.blocks() as f64;
    if blocks == 0.0 {
        return Ok(0.0);
    }
    Ok(((blocks - st.blocks_free() as f64) * 100.0 / blocks) as f32)
}

/// Host and port to probe for an Immich base URL such as
/// `https://immich.example/api` → `("immich.example", 443)`.
pub fn server_addr(base_url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = base_url.trim().split_once("://")?;
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "https" => 443,
        "http" => 80,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?; // drop userinfo
    if authority.is_empty() {
        return None;
    }
    if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if after.is_empty() => default_port,
            None => return None,
        };
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((authority.to_string(), default_port)),
    }
}

/// TCP reachability of `host:port` within `timeout`.
pub fn reachable(host: &str, port: u16, timeout: Duration) -> bool {
    let Ok(addrs) = (host, port).to_socket_addrs() else { return false };
    addrs.into_iter().any(|a| TcpStream::connect_timeout(&a, timeout).is_ok())
}

pub struct Monitors {
    status: Arc<Mutex<Status>>,
    running: Arc<AtomicBool>,
    /// The local-checks thread; joined on drop (it wakes every 100 ms).
    local: Option<JoinHandle<()>>,
}

impl Monitors {
    pub fn start(cfg: &Config) -> anyhow::Result<Self> {
        let status = Arc::new(Mutex::new(Status::default()));
        let running = Arc::new(AtomicBool::new(true));
        let (st, run, mon, root) = (status.clone(), running.clone(), cfg.monitor.clone(), cfg.storage.root.clone());
        let local = std::thread::Builder::new().name("monitor".into()).spawn(move || {
            local_loop(&st, &run, &mon, &root);
        })?;

        if let Some((host, port)) = server_addr(&cfg.immich.base_url) {
            status.lock().unwrap().network = Network::Unknown;
            let (st, run) = (status.clone(), running.clone());
            // Detached: a connect can block for NET_TIMEOUT, which must not delay quitting.
            std::thread::Builder::new().name("net-monitor".into()).spawn(move || {
                net_loop(&st, &run, &host, port);
            })?;
        } else if !cfg.immich.base_url.is_empty() {
            tracing::warn!(url = %cfg.immich.base_url, "cannot parse immich.base_url; network check disabled");
        }
        Ok(Self { status, running, local: Some(local) })
    }

    pub fn snapshot(&self) -> Status {
        *self.status.lock().unwrap()
    }
}

impl Drop for Monitors {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(t) = self.local.take() {
            let _ = t.join();
        }
    }
}

/// Sleep up to `d`, waking early when `running` clears. Returns false to stop.
fn nap(running: &AtomicBool, d: Duration) -> bool {
    let end = Instant::now() + d;
    while running.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now >= end {
            return true;
        }
        std::thread::sleep((end - now).min(Duration::from_millis(100)));
    }
    false
}

fn local_loop(status: &Mutex<Status>, running: &AtomicBool, mon: &crate::config::Monitor, root: &Path) {
    let mut next_disk = Instant::now();
    loop {
        let temp_c = std::fs::read_to_string(TEMP_PATH).ok().and_then(|t| parse_millidegrees(&t));
        let disk = if Instant::now() >= next_disk {
            next_disk = Instant::now() + DISK_INTERVAL;
            match disk_used_pct(root) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::warn!("disk usage of {}: {e}", root.display());
                    None
                }
            }
        } else {
            None
        };
        {
            let mut s = status.lock().unwrap();
            s.temp_c = temp_c;
            s.temp = temp_c.map_or(Level::Ok, |t| classify(t, mon.temp_warn_c, mon.temp_crit_c));
            if let Some(p) = disk {
                s.disk_pct = Some(p);
                s.disk = classify(p, mon.disk_warn_pct, mon.disk_crit_pct);
            }
        }
        if !nap(running, TEMP_INTERVAL) {
            return;
        }
    }
}

fn net_loop(status: &Mutex<Status>, running: &AtomicBool, host: &str, port: u16) {
    loop {
        let online = reachable(host, port, NET_TIMEOUT);
        let previous = std::mem::replace(
            &mut status.lock().unwrap().network,
            if online { Network::Online } else { Network::Offline },
        );
        if previous != Network::Online && online {
            tracing::info!(host, port, "Immich server reachable");
        } else if previous != Network::Offline && !online {
            tracing::warn!(host, port, "Immich server unreachable");
        }
        if !nap(running, if online { NET_INTERVAL_ONLINE } else { NET_INTERVAL_OFFLINE }) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_matches_v1_thresholds() {
        assert_eq!(classify(55.0, 60.0, 65.0), Level::Ok);
        assert_eq!(classify(60.0, 60.0, 65.0), Level::Ok, "warning is strictly above");
        assert_eq!(classify(60.1, 60.0, 65.0), Level::Warning);
        assert_eq!(classify(65.0, 60.0, 65.0), Level::Critical);
        assert_eq!(classify(91.0, 70.0, 90.0), Level::Critical);
    }

    #[test]
    fn parses_thermal_zone() {
        assert_eq!(parse_millidegrees("48312\n"), Some(48.312));
        assert_eq!(parse_millidegrees("garbage"), None);
    }

    #[test]
    fn disk_usage_of_current_dir_is_a_percentage() {
        let p = disk_used_pct(Path::new(".")).unwrap();
        assert!((0.0..=100.0).contains(&p), "{p}");
        assert!(disk_used_pct(Path::new("/nonexistent/dir")).is_err());
    }

    #[test]
    fn server_addr_from_urls() {
        assert_eq!(server_addr("https://immich.example/api"), Some(("immich.example".into(), 443)));
        assert_eq!(server_addr("http://192.168.1.10:2283/api"), Some(("192.168.1.10".into(), 2283)));
        assert_eq!(server_addr("HTTPS://user:pw@host.lan"), Some(("host.lan".into(), 443)));
        assert_eq!(server_addr("https://[fd00::1]:8443/api"), Some(("fd00::1".into(), 8443)));
        assert_eq!(server_addr("https://[fd00::1]/api"), Some(("fd00::1".into(), 443)));
        assert_eq!(server_addr(""), None);
        assert_eq!(server_addr("immich.example/api"), None);
        assert_eq!(server_addr("ftp://x"), None);
        assert_eq!(server_addr("https://host:notaport"), None);
    }

    #[test]
    fn reachability_against_local_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(reachable("127.0.0.1", port, Duration::from_secs(1)));
        drop(listener);
        assert!(!reachable("127.0.0.1", port, Duration::from_millis(500)));
    }

    #[test]
    fn monitors_publish_disk_and_disabled_network() {
        let cfg = Config::default();
        let m = Monitors::start(&cfg).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while m.snapshot().disk_pct.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let s = m.snapshot();
        assert!(s.disk_pct.is_some());
        assert_eq!(s.network, Network::Disabled);
    }
}
