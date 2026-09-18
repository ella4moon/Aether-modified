// CI drives the exact production packet checks against a temporary /32 TUN.
// Print the configured probe, then wait until the caller has started sing-box.
use aether_whole_laptop::{check_dns, CaptureProbe};
use std::io::{self, Read, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let config_path = args.next().ok_or("Pass a base config path")?;
    let dns = args.next().ok_or("Pass the test TUN DNS address")?.parse()?;
    let mut config = serde_json::from_slice(&std::fs::read(config_path)?)?;
    let probe = CaptureProbe::new()?;
    probe.configure(&mut config)?;
    println!("{}", serde_json::to_string(&config)?);
    io::stdout().flush()?;
    io::stdin().read_exact(&mut [0])?;
    probe.verify()?;
    println!("PASS: production TCP capture and return-traffic check");
    check_dns(dns)?;
    println!("PASS: production DNS response check through the SOCKS outbound");
    Ok(())
}
