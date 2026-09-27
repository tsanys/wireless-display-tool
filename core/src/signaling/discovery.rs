//! mDNS advertising sender via mdns-sd (T3.4 penuh menyusul).
//!
//! Mengiklankan `_wdt._tcp.local.` dengan TXT `proto` + `ver` sesuai
//! `docs/SIGNALING_PROTOCOL.md`.

use mdns_sd::{ServiceDaemon, ServiceInfo};

use super::protocol::{MDNS_SERVICE_TYPE, PROTO_VERSION};

/// Handle iklan mDNS yang aktif; drop = unregister otomatis.
pub struct MdnsAdvert {
    daemon: ServiceDaemon,
    fullname: String,
}

impl MdnsAdvert {
    /// Daftarkan service instance `instance` (mis. "WDT MacBook-Pro")
    /// pada `host` + `ip` + `port`.
    pub fn advertise(
        instance: &str,
        host: &str,
        ip: &str,
        port: u16,
    ) -> Result<Self, mdns_sd::Error> {
        let daemon = ServiceDaemon::new()?;
        let props = [
            ("proto", PROTO_VERSION.to_string()),
            ("ver", env!("CARGO_PKG_VERSION").to_string()),
        ];
        // ServiceInfo::new memvalidasi nama & membuat TXT record.
        let info = ServiceInfo::new(MDNS_SERVICE_TYPE, instance, host, ip, port, &props[..])?;
        let fullname = info.get_fullname().to_string();
        daemon.register(info)?;
        Ok(Self { daemon, fullname })
    }

    /// Fullname terdaftar, mis. "WDT MacBook-Pro._wdt._tcp.local."
    pub fn fullname(&self) -> &str {
        &self.fullname
    }

    /// Hentikan iklan secara eksplisit (drop juga shutdown daemon).
    pub fn stop(self) {
        let _ = self.daemon.shutdown();
    }
}
