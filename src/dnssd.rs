use anyhow::Result;

fn txt(entries: &[(&str, &str)]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for (k, v) in entries {
        let entry = format!("{k}={v}");
        anyhow::ensure!(entry.len() < 256, "TXT entry {k} too long");
        out.push(entry.len() as u8);
        out.extend_from_slice(entry.as_bytes());
    }
    Ok(out)
}

#[cfg(target_os = "macos")]
pub struct Advert(*mut std::ffi::c_void);

#[cfg(target_os = "macos")]
unsafe impl Send for Advert {}
#[cfg(target_os = "macos")]
unsafe impl Sync for Advert {}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn DNSServiceRegister(
        sd_ref: *mut *mut std::ffi::c_void,
        flags: u32,
        interface_index: u32,
        name: *const std::ffi::c_char,
        regtype: *const std::ffi::c_char,
        domain: *const std::ffi::c_char,
        host: *const std::ffi::c_char,
        port: u16,
        txt_len: u16,
        txt_record: *const std::ffi::c_void,
        callback: *const std::ffi::c_void,
        context: *mut std::ffi::c_void,
    ) -> i32;
    fn DNSServiceRefDeallocate(sd_ref: *mut std::ffi::c_void);
}

#[cfg(target_os = "macos")]
impl Advert {
    pub fn new(_: &mdns_sd::ServiceDaemon, service: &str, instance: &str, port: u16, entries: &[(&str, &str)]) -> Result<Self> {
        let txt = txt(entries)?;
        let name = std::ffi::CString::new(instance)?;
        let ty = std::ffi::CString::new(service.trim_end_matches('.').trim_end_matches(".local"))?;
        let mut sd = std::ptr::null_mut();
        let err = unsafe {
            DNSServiceRegister(
                &mut sd,
                0,
                0,
                name.as_ptr(),
                ty.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                port.to_be(),
                txt.len() as u16,
                txt.as_ptr().cast(),
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        anyhow::ensure!(err == 0 && !sd.is_null(), "DNSServiceRegister failed ({err})");
        Ok(Advert(sd))
    }
}

#[cfg(target_os = "macos")]
impl Drop for Advert {
    fn drop(&mut self) {
        unsafe { DNSServiceRefDeallocate(self.0) }
    }
}

#[cfg(not(target_os = "macos"))]
pub struct Advert;

#[cfg(not(target_os = "macos"))]
impl Advert {
    pub fn new(mdns: &mdns_sd::ServiceDaemon, service: &str, instance: &str, port: u16, entries: &[(&str, &str)]) -> Result<Self> {
        txt(entries)?;
        let host: String = instance.chars().filter(|c| c.is_ascii_alphanumeric()).take(32).collect::<String>().to_lowercase();
        let hostname = format!("hither-{host}.local.");
        let svc = mdns_sd::ServiceInfo::new(service, instance, &hostname, "", port, entries)?.enable_addr_auto();
        mdns.register(svc)?;
        Ok(Advert)
    }
}
