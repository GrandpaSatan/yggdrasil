use std::process::Command;

use anyhow::{Context, Result};
use tracing::info;

/// Detected operating system.
#[derive(Debug, Clone)]
pub enum Os {
    Linux,
    MacOs,
    Windows,
    Unknown(String),
}

impl std::fmt::Display for Os {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Linux => write!(f, "linux"),
            Self::MacOs => write!(f, "macos"),
            Self::Windows => write!(f, "windows"),
            Self::Unknown(s) => write!(f, "unknown({s})"),
        }
    }
}

/// Detect the current operating system.
pub fn detect_os() -> Os {
    if cfg!(target_os = "linux") {
        Os::Linux
    } else if cfg!(target_os = "macos") {
        Os::MacOs
    } else if cfg!(target_os = "windows") {
        Os::Windows
    } else {
        Os::Unknown(std::env::consts::OS.to_string())
    }
}

/// Ensure data and binary directories exist.
pub fn ensure_dirs(data_dir: &str, bin_dir: &str) -> Result<()> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("failed to create data dir: {data_dir}"))?;
    std::fs::create_dir_all(bin_dir)
        .with_context(|| format!("failed to create bin dir: {bin_dir}"))?;
    std::fs::create_dir_all(format!("{data_dir}/configs"))
        .context("failed to create configs dir")?;
    std::fs::create_dir_all(format!("{data_dir}/logs"))
        .context("failed to create logs dir")?;
    Ok(())
}

/// Install a single service based on the OS.
pub fn install_service(name: &str, bin_dir: &str, data_dir: &str, os: &Os) -> Result<()> {
    match os {
        Os::Linux => install_systemd_service(name, bin_dir, data_dir),
        Os::MacOs => install_launchd_service(name, bin_dir, data_dir),
        Os::Windows => install_windows_service(name, bin_dir, data_dir),
        Os::Unknown(_) => anyhow::bail!("unsupported OS for service installation"),
    }
}

/// Generate and install a systemd unit file.
fn install_systemd_service(name: &str, bin_dir: &str, data_dir: &str) -> Result<()> {
    let unit = format!(
        r#"[Unit]
Description=Yggdrasil {name}
After=network.target

[Service]
Type=notify
ExecStart={bin_dir}/{name} --config {data_dir}/configs/{name}/config.json
Restart=on-failure
RestartSec=5
User=yggdrasil
Group=yggdrasil
WorkingDirectory={data_dir}
WatchdogSec=60

[Install]
WantedBy=multi-user.target
"#
    );

    let unit_path = format!("/etc/systemd/system/ygg-{name}.service");
    info!(path = %unit_path, "writing systemd unit");

    std::fs::write(&unit_path, &unit)
        .with_context(|| format!("failed to write {unit_path}"))?;

    // Reload systemd and enable
    Command::new("systemctl").args(["daemon-reload"]).status()?;
    Command::new("systemctl")
        .args(["enable", &format!("ygg-{name}")])
        .status()?;

    Ok(())
}

/// Generate and install a macOS launchd plist.
fn install_launchd_service(name: &str, bin_dir: &str, data_dir: &str) -> Result<()> {
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.yggdrasil.{name}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin_dir}/{name}</string>
        <string>--config</string>
        <string>{data_dir}/configs/{name}/config.json</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>WorkingDirectory</key>
    <string>{data_dir}</string>
    <key>StandardOutPath</key>
    <string>{data_dir}/logs/{name}.log</string>
    <key>StandardErrorPath</key>
    <string>{data_dir}/logs/{name}.err</string>
</dict>
</plist>
"#
    );

    let plist_path = format!("/Library/LaunchDaemons/com.yggdrasil.{name}.plist");
    info!(path = %plist_path, "writing launchd plist");

    std::fs::write(&plist_path, &plist)
        .with_context(|| format!("failed to write {plist_path}"))?;

    Ok(())
}

/// Install a Windows service via sc.exe.
fn install_windows_service(name: &str, bin_dir: &str, data_dir: &str) -> Result<()> {
    let bin_path = format!("{bin_dir}\\{name}.exe");
    let svc_name = format!("ygg-{name}");
    let config_path = format!("{data_dir}\\configs\\{name}\\config.json");

    info!(name = %svc_name, bin = %bin_path, "installing Windows service");

    // Create the service using sc.exe
    let output = Command::new("sc.exe")
        .args([
            "create",
            &svc_name,
            &format!("binPath= \"{bin_path}\" --config \"{config_path}\""),
            "start=", "auto",
            &format!("DisplayName= Yggdrasil {name}"),
        ])
        .output()
        .with_context(|| format!("failed to run sc.exe for {svc_name}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Service might already exist — try configuring it
        if !stderr.contains("1073") {
            // ERROR_SERVICE_EXISTS
            anyhow::bail!("sc.exe create failed: {stderr}");
        }
        info!(name = %svc_name, "service already exists — reconfiguring");
    }

    // Set the description
    let _ = Command::new("sc.exe")
        .args(["description", &svc_name, &format!("Yggdrasil {name} service")])
        .status();

    // Set recovery options: restart on failure
    let _ = Command::new("sc.exe")
        .args([
            "failure", &svc_name,
            "reset=", "86400",
            "actions=", "restart/5000/restart/10000/restart/30000",
        ])
        .status();

    Ok(())
}

/// Uninstall all Yggdrasil services.
pub fn uninstall_all(os: &Os) -> Result<()> {
    let services = [
        "odin", "mimir", "huginn", "muninn", "ygg-node", "ygg-sentinel", "ygg-voice", "ygg-agent",
    ];

    match os {
        Os::Linux => {
            for svc in &services {
                let unit = format!("ygg-{svc}");
                let _ = Command::new("systemctl").args(["stop", &unit]).status();
                let _ = Command::new("systemctl").args(["disable", &unit]).status();
                let path = format!("/etc/systemd/system/{unit}.service");
                let _ = std::fs::remove_file(&path);
            }
            let _ = Command::new("systemctl").args(["daemon-reload"]).status();
        }
        Os::MacOs => {
            for svc in &services {
                let plist_path = format!("/Library/LaunchDaemons/com.yggdrasil.{svc}.plist");
                let _ = Command::new("launchctl").args(["unload", &plist_path]).status();
                let _ = std::fs::remove_file(&plist_path);
            }
        }
        Os::Windows => {
            for svc in &services {
                let svc_name = format!("ygg-{svc}");
                let _ = Command::new("sc.exe").args(["stop", &svc_name]).status();
                let _ = Command::new("sc.exe").args(["delete", &svc_name]).status();
            }
        }
        Os::Unknown(_) => {
            info!("uninstall not supported on this OS");
        }
    }

    Ok(())
}

/// Get status of all Yggdrasil services.
pub fn service_status(os: &Os) -> Result<Vec<(String, String)>> {
    let services = [
        "odin", "mimir", "huginn", "muninn", "ygg-node", "ygg-sentinel", "ygg-voice", "ygg-agent",
    ];
    let mut results = Vec::new();

    match os {
        Os::Linux => {
            for svc in &services {
                let unit = format!("ygg-{svc}");
                let output = Command::new("systemctl")
                    .args(["is-active", &unit])
                    .output();

                let status = match output {
                    Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
                    Err(_) => "unknown".to_string(),
                };
                results.push((svc.to_string(), status));
            }
        }
        Os::MacOs => {
            for svc in &services {
                let plist_path = format!("/Library/LaunchDaemons/com.yggdrasil.{svc}.plist");
                if !std::path::Path::new(&plist_path).exists() {
                    results.push((svc.to_string(), "not installed".to_string()));
                    continue;
                }
                let output = Command::new("launchctl")
                    .args(["list", &format!("com.yggdrasil.{svc}")])
                    .output();
                let status = match output {
                    Ok(o) if o.status.success() => "running".to_string(),
                    Ok(_) => "stopped".to_string(),
                    Err(_) => "unknown".to_string(),
                };
                results.push((svc.to_string(), status));
            }
        }
        Os::Windows => {
            for svc in &services {
                let svc_name = format!("ygg-{svc}");
                let output = Command::new("sc.exe")
                    .args(["query", &svc_name])
                    .output();
                let status = match output {
                    Ok(o) => {
                        let stdout = String::from_utf8_lossy(&o.stdout);
                        if stdout.contains("RUNNING") {
                            "running".to_string()
                        } else if stdout.contains("STOPPED") {
                            "stopped".to_string()
                        } else if o.status.success() {
                            "unknown".to_string()
                        } else {
                            "not installed".to_string()
                        }
                    }
                    Err(_) => "unknown".to_string(),
                };
                results.push((svc.to_string(), status));
            }
        }
        Os::Unknown(_) => {
            for svc in &services {
                results.push((svc.to_string(), "unsupported OS".to_string()));
            }
        }
    }

    Ok(results)
}
