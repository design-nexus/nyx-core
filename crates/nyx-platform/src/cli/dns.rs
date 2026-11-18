//! `dns [Cloudflare|Google|DHCP|Custom]`: the DNS servers, on every Wi-Fi and wired
//! connection through NetworkManager (no root needed). Without an argument, prints the
//! provider in use; `Custom` reads the servers from standard input.

use super::{Fail, Res, nm_get, run};
use std::io::BufRead;

const CLOUDFLARE: &[&str] = &["1.1.1.1", "1.0.0.1", "2606:4700:4700::1111", "2606:4700:4700::1001"];
const GOOGLE: &[&str] = &["8.8.8.8", "8.8.4.4", "2001:4860:4860::8888", "2001:4860:4860::8844"];

/// What a connection's servers amount to: `DHCP` when it takes the network's.
pub fn provider(ignore_auto: bool, servers: &[String]) -> &'static str {
    if !ignore_auto || servers.is_empty() {
        "DHCP"
    } else if servers.iter().all(|s| CLOUDFLARE.contains(&s.as_str())) {
        "Cloudflare"
    } else if servers.iter().all(|s| GOOGLE.contains(&s.as_str())) {
        "Google"
    } else {
        "Custom"
    }
}

/// Servers in any of `1.1.1.1,1.0.0.1`, `1.1.1.1 1.0.0.1` or one per line.
pub fn split(text: &str) -> Vec<String> {
    text.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// The Wi-Fi and wired connections: (uuid, device it's active on, if any).
fn connections() -> Vec<(String, String)> {
    nm_get(&["UUID,TYPE,DEVICE", "connection", "show"])
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() >= 3 && matches!(f[1], "802-11-wireless" | "802-3-ethernet")).then(|| (f[0].to_string(), f[2].to_string()))
        })
        .collect()
}

fn current() -> &'static str {
    // The active connection's; else the first one's.
    let all = connections();
    let Some((uuid, _)) = all.iter().find(|(_, d)| !d.is_empty()).or(all.first()) else { return "DHCP" };
    let v = nm_get(&["ipv4.dns,ipv4.ignore-auto-dns,ipv6.dns", "connection", "show", uuid]);
    let lines: Vec<&str> = v.lines().collect();
    let mut servers = split(lines.first().copied().unwrap_or(""));
    servers.extend(split(lines.get(2).copied().unwrap_or("")));
    provider(lines.get(1).is_some_and(|l| l.trim() == "yes"), &servers)
}

fn set(servers: &[String]) -> Res {
    let v4: Vec<&str> = servers.iter().filter(|s| !s.contains(':')).map(String::as_str).collect();
    let v6: Vec<&str> = servers.iter().filter(|s| s.contains(':')).map(String::as_str).collect();
    let auto = if servers.is_empty() { "no" } else { "yes" };
    let all = connections();
    if all.is_empty() {
        return Err("no Wi-Fi or wired connections to set.".into());
    }
    for (uuid, _) in &all {
        run(
            "nmcli",
            &[
                "connection",
                "modify",
                uuid,
                "ipv4.dns",
                &v4.join(" "),
                "ipv4.ignore-auto-dns",
                auto,
                "ipv6.dns",
                &v6.join(" "),
                "ipv6.ignore-auto-dns",
                auto,
            ],
        )?;
    }
    // The connections in use take the servers now.
    for (_, device) in all.iter().filter(|(_, d)| !d.is_empty()) {
        let _ = run("nmcli", &["device", "reapply", device]);
    }
    Ok(())
}

pub fn run_dns(args: &[&str]) -> Res {
    match args {
        [] => {
            println!("{}", current());
            Ok(())
        }
        [p] => match p.to_ascii_lowercase().as_str() {
            "cloudflare" => set(&CLOUDFLARE.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
            "google" => set(&GOOGLE.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
            "dhcp" => set(&[]),
            "custom" => {
                println!("Enter your DNS servers (space-separated, e.g. '192.168.1.1 1.1.1.1'):");
                let mut line = String::new();
                std::io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
                let servers = split(&line);
                if servers.is_empty() {
                    return Err("No DNS servers provided.".into());
                }
                set(&servers)
            }
            _ => Err(Fail::Usage("[Cloudflare|Google|DHCP|Custom]")),
        },
        _ => Err(Fail::Usage("[Cloudflare|Google|DHCP|Custom]")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_name_their_provider() {
        let s = |t: &str| split(t);
        assert_eq!(provider(true, &s("1.1.1.1,1.0.0.1")), "Cloudflare");
        assert_eq!(provider(true, &s("8.8.8.8 2001:4860:4860::8888")), "Google");
        assert_eq!(provider(true, &s("192.168.1.1")), "Custom");
        assert_eq!(provider(false, &s("1.1.1.1")), "DHCP");
        assert_eq!(provider(true, &[]), "DHCP");
    }
}
