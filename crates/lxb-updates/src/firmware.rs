//! Firmware installation is deliberately narrower than fwupd discovery. Never
//! call `fwupdmgr update` without an explicit, freshly classified device ID.
//! Unknown protocols and platform firmware remain manual-maintenance items.
use crate::{process, Item};
use anyhow::{bail, Context, Result};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub protocol: String,
    pub version: String,
}

fn strings(value: &Value) -> Vec<&str> {
    match value {
        Value::String(s) => vec![s],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    }
}

pub fn classify(device: &Value) -> Result<Device, &'static str> {
    let plugin = device["Plugin"].as_str().unwrap_or("").to_ascii_lowercase();
    let name = device["Name"].as_str().unwrap_or("").to_ascii_lowercase();
    let protocols = if device.get("Protocols").is_some() {
        strings(&device["Protocols"])
    } else {
        strings(&device["Protocol"])
    };
    let releases = device["Releases"].as_array();
    let platform_category = releases
        .into_iter()
        .flatten()
        .flat_map(|r| strings(&r["Categories"]))
        .any(|c| {
            matches!(
                c,
                "X-System"
                    | "X-EmbeddedController"
                    | "X-ManagementEngine"
                    | "X-PlatformSecurityProcessor"
            )
        });
    if platform_category
        || protocols.iter().any(|p| p.starts_with("org.uefi"))
        || [
            "uefi",
            "bios",
            "flashrom",
            "coreboot",
            "intel_me",
            "intel_spi",
            "amd_psp",
        ]
        .iter()
        .any(|s| plugin.contains(s))
        || [
            "bios",
            "uefi",
            "system firmware",
            "embedded controller",
            "management engine",
        ]
        .iter()
        .any(|s| name.contains(s))
    {
        return Err("BIOS/UEFI or platform firmware — manual update required");
    }
    // Extend only with fixtures and device validation. A negative BIOS-name
    // match is not permission to flash arbitrary hardware.
    if protocols.len() != 1
        || !matches!(
            protocols[0],
            "org.nvmexpress"
                | "org.t13.ata"
                | "com.logitech.unifying"
                | "com.logitech.hidpp"
                | "com.hughski.colorhug"
        )
    {
        return Err("Device type is not yet validated — manual update required");
    }
    let flags = strings(&device["Flags"]);
    if !flags.contains(&"updatable")
        || flags.contains(&"is-bootloader")
        || flags.contains(&"locked")
    {
        return Err("Device is not eligible for routine firmware installation");
    }
    let id = device["DeviceId"]
        .as_str()
        .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("Unrecognized device identity — manual update required")?;
    Ok(Device {
        id: id.into(),
        name: device["Name"].as_str().unwrap_or("Firmware device").into(),
        protocol: protocols[0].into(),
        version: device["Version"].as_str().unwrap_or("Unknown").into(),
    })
}

fn walk<'a>(devices: &'a [Value], out: &mut Vec<&'a Value>) {
    for d in devices {
        out.push(d);
        if let Some(children) = d["Children"].as_array() {
            walk(children, out);
        }
    }
}

pub fn parse(text: &str) -> Result<Vec<Value>> {
    let json: Value = serde_json::from_str(text).context("fwupd did not return valid JSON")?;
    if let Some(error) = json.get("Error") {
        bail!("fwupd: {error}");
    }
    let list = json["Devices"]
        .as_array()
        .context("fwupd did not return its device list")?;
    let mut all = vec![];
    walk(list, &mut all);
    Ok(all.into_iter().cloned().collect())
}

pub fn devices() -> Result<Vec<Value>> {
    parse(&process::probe("fwupdmgr", &["get-devices", "--json"], &[0])?.text)
}

/// Refresh only already configured remotes. JSON disables interaction, and the
/// explicit remote flag prevents offering to enable a new firmware source.
/// Exit 2 means nothing was downloaded; retain the cached-metadata label.
pub fn refresh() -> Result<bool> {
    let output = process::probe(
        "fwupdmgr",
        &[
            "refresh",
            "--json",
            "--no-remote-check",
            "--no-unreported-check",
        ],
        &[0, 2],
    )?;
    Ok(output.code == 0)
}

pub fn inventory() -> Result<(Vec<Item>, Vec<Item>)> {
    let mut updates = vec![];
    let mut excluded = vec![];
    for raw in devices()? {
        let name = raw["Name"]
            .as_str()
            .unwrap_or("Unidentified device")
            .to_owned();
        match classify(&raw) {
            Ok(device) => {
                let output =
                    process::probe("fwupdmgr", &["get-updates", &device.id, "--json"], &[0, 2])?;
                if output.code == 2 {
                    continue;
                }
                let listed = parse(&output.text)?;
                for d in listed {
                    if d["DeviceId"].as_str() == Some(&device.id)
                        && d["Releases"].as_array().is_some_and(|r| !r.is_empty())
                    {
                        // Classify again with release categories now available.
                        match classify(&d) {
                            Ok(_) => updates.push(Item {
                                name: device.name.clone(),
                                detail: format!(
                                    "{} · {} · {}",
                                    device.id, device.protocol, device.version
                                ),
                            }),
                            Err(reason) => excluded.push(Item {
                                name: name.clone(),
                                detail: reason.into(),
                            }),
                        }
                    }
                }
            }
            Err(reason) => excluded.push(Item {
                name,
                detail: reason.into(),
            }),
        }
    }
    Ok((updates, excluded))
}

/// Re-read both device and release metadata immediately before starting a
/// targeted transaction. An old UI classification is never an authorization.
pub fn installation(id: &str) -> Result<process::Step> {
    let all = devices()?;
    let raw = all
        .iter()
        .find(|d| d["DeviceId"].as_str() == Some(id))
        .context("The firmware device disconnected")?;
    let device = classify(raw).map_err(|s| anyhow::anyhow!(s))?;
    let release = process::probe("fwupdmgr", &["get-updates", &device.id, "--json"], &[0])?;
    let entries = parse(&release.text)?;
    let raw = entries
        .iter()
        .find(|d| d["DeviceId"].as_str() == Some(id))
        .context("No update for this device")?;
    let current = classify(raw).map_err(|s| anyhow::anyhow!(s))?;
    if current.protocol != device.protocol {
        bail!("The firmware device changed; check again");
    }
    // `-y` answers "Perform operation? [Y|n]", which Update now already
    // answered; the reboot question it would also answer is never asked,
    // because `--no-reboot-check` takes it away. No force, remote changes,
    // history upload, reboot or unsigned/downgrade overrides.
    Ok(process::Step::new(
        "fwupdmgr",
        &[
            "update",
            id,
            "-y",
            "--filter-protocol",
            &device.protocol,
            "--no-reboot-check",
            "--no-unreported-check",
            "--no-remote-check",
        ],
        false,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn disk() -> Value {
        serde_json::json!({"DeviceId":"0123456789012345678901234567890123456789", "Name":"SSD", "Plugin":"nvme", "Protocol":"org.nvmexpress", "Flags":["internal", "updatable"]})
    }
    #[test]
    fn only_known_device_protocols_can_be_installed() {
        assert!(classify(&disk()).is_ok());
        let mut ata = disk();
        ata["Plugin"] = "ata".into();
        ata["Protocol"] = "org.t13.ata".into();
        assert!(classify(&ata).is_ok());
        for protocol in [
            "org.uefi.capsule",
            "org.uefi.dbx",
            "org.usb.dfu",
            "future.protocol",
            "",
        ] {
            let mut d = disk();
            d["Protocol"] = protocol.into();
            assert!(classify(&d).is_err());
        }
    }
    #[test]
    fn misleading_names_never_override_platform_metadata() {
        let mut d = disk();
        d["Plugin"] = "uefi_capsule".into();
        assert!(classify(&d).is_err());
        d = disk();
        d["Releases"] = serde_json::json!([{"Categories":["X-System"]}]);
        assert!(classify(&d).is_err());
        d = disk();
        d["DeviceId"] = "--force".into();
        assert!(classify(&d).is_err());
        d = disk();
        d["Flags"] = serde_json::json!(["updatable", "is-bootloader"]);
        assert!(classify(&d).is_err());
        d = disk();
        d["Protocols"] = serde_json::json!(["org.nvmexpress", "org.uefi.capsule"]);
        assert!(classify(&d).is_err());
    }
    #[test]
    fn errors_are_not_empty_update_lists() {
        assert!(parse(r#"{"Error":{"Message":"daemon unavailable"}}"#).is_err());
        assert!(parse("{}").is_err());
    }
    #[test]
    fn discovery_visits_nested_devices() {
        assert_eq!(
            parse(r#"{"Devices":[{"Children":[{}]}]}"#).unwrap().len(),
            2
        );
    }
}
