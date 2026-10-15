//! Command-line arguments for the client (hand-rolled to keep deps small).

use anyhow::{bail, Result};
use macss_protocol::{Codec, DEFAULT_CONTROL_PORT};

#[derive(Debug, Clone)]
pub struct Args {
    pub host: String,
    pub control_port: u16,
    /// 0 means "pick an ephemeral port"; the host is told via `start_stream`.
    pub video_port: u16,
    pub token: Option<String>,
    pub client_name: String,
    pub codec: Codec,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub wol: Option<[u8; 6]>,
    pub wol_port: u16,
    /// Log a summary line every N received frames (0 disables).
    pub log_every: u64,
    /// Run without a window (no rendering, no input capture).
    pub headless: bool,
    /// Generate local test frames instead of relying on the decoder.
    pub synthetic_video: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            host: String::new(),
            control_port: DEFAULT_CONTROL_PORT,
            video_port: 0,
            token: None,
            client_name: hostname(),
            codec: Codec::H264,
            fps: 60,
            bitrate_kbps: 20_000,
            wol: None,
            wol_port: 9,
            log_every: 120,
            headless: false,
            synthetic_video: false,
        }
    }
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "client".to_string())
}

pub const USAGE: &str = "\
macss-client — connect to a macscreenshare host

USAGE:
    macss-client <HOST> [OPTIONS]

ARGS:
    <HOST>                 Host name or IP (port may be given as host:port)

OPTIONS:
    --control-port <P>     TCP control port           [default: 47810]
    --video-port <P>       Local UDP port, 0 = ephemeral [default: 0]
    --token <T>            Shared secret, if the host requires one
    --name <N>             Client name reported to the host
    --codec <h264|hevc>    Preferred codec            [default: h264]
    --fps <N>              Requested frame rate       [default: 60]
    --bitrate <KBPS>       Max bitrate in kbit/s      [default: 20000]
    --wol <MAC>            Send a magic packet before connecting (aa:bb:cc:dd:ee:ff)
    --wol-port <P>         Wake-on-LAN port           [default: 9]
    --log-every <N>        Log every N frames, 0 = off [default: 120]
    --headless             No window; just connect and log
    --synthetic-video      Generate local test frames (no decoder needed)
    -h, --help             Show this help
";

pub fn parse_from<I: IntoIterator<Item = String>>(iter: I) -> Result<Option<Args>> {
    let mut args = Args::default();
    let mut positional: Vec<String> = Vec::new();
    let mut it = iter.into_iter();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--control-port" => args.control_port = next_num(&mut it, "--control-port")?,
            "--video-port" => args.video_port = next_num(&mut it, "--video-port")?,
            "--token" => args.token = Some(next_val(&mut it, "--token")?),
            "--name" => args.client_name = next_val(&mut it, "--name")?,
            "--codec" => {
                let v = next_val(&mut it, "--codec")?;
                args.codec = match v.to_ascii_lowercase().as_str() {
                    "h264" | "avc" => Codec::H264,
                    "hevc" | "h265" => Codec::Hevc,
                    other => bail!("unknown codec {other:?} (expected h264 or hevc)"),
                };
            }
            "--fps" => args.fps = next_num(&mut it, "--fps")?,
            "--bitrate" => args.bitrate_kbps = next_num(&mut it, "--bitrate")?,
            "--wol" => args.wol = Some(parse_mac(&next_val(&mut it, "--wol")?)?),
            "--wol-port" => args.wol_port = next_num(&mut it, "--wol-port")?,
            "--log-every" => args.log_every = next_num(&mut it, "--log-every")?,
            "--headless" => args.headless = true,
            "--synthetic-video" => args.synthetic_video = true,
            other if other.starts_with("--") => bail!("unknown option {other:?}"),
            other => positional.push(other.to_string()),
        }
    }

    if positional.is_empty() {
        bail!("missing <HOST> (use --help for usage)");
    }
    let target = positional.remove(0);

    // Split an optional ":port" off the host.
    if let Some((h, p)) = split_host_port(&target) {
        args.host = h;
        args.control_port = p;
    } else {
        args.host = target;
    }

    if args.fps == 0 || args.fps > 240 {
        bail!("--fps must be between 1 and 240");
    }
    Ok(Some(args))
}

fn split_host_port(s: &str) -> Option<(String, u16)> {
    // IPv6 in brackets: [::1]:1234
    if let Some(rest) = s.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = tail.strip_prefix(':')?.parse().ok()?;
        return Some((host.to_string(), port));
    }
    let (host, port) = s.rsplit_once(':')?;
    if host.contains(':') {
        return None; // bare IPv6, no port
    }
    Some((host.to_string(), port.parse().ok()?))
}

fn next_val<I: Iterator<Item = String>>(it: &mut I, flag: &str) -> Result<String> {
    it.next().ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}

fn next_num<T: std::str::FromStr, I: Iterator<Item = String>>(it: &mut I, flag: &str) -> Result<T> {
    let v = next_val(it, flag)?;
    v.parse()
        .map_err(|_| anyhow::anyhow!("{flag}: invalid value {v:?}"))
}

pub fn parse_mac(s: &str) -> Result<[u8; 6]> {
    let parts: Vec<&str> = s.split([':', '-']).collect();
    if parts.len() != 6 {
        bail!("MAC address must have 6 groups, got {:?}", s);
    }
    let mut mac = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        mac[i] = u8::from_str_radix(p, 16)
            .map_err(|_| anyhow::anyhow!("invalid hex byte {p:?} in MAC {s:?}"))?;
    }
    Ok(mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: &[&str]) -> Result<Option<Args>> {
        parse_from(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn host_only() {
        let a = parse(&["10.0.0.5"]).unwrap().unwrap();
        assert_eq!(a.host, "10.0.0.5");
        assert_eq!(a.control_port, DEFAULT_CONTROL_PORT);
    }

    #[test]
    fn host_with_port() {
        let a = parse(&["10.0.0.5:9999"]).unwrap().unwrap();
        assert_eq!(a.host, "10.0.0.5");
        assert_eq!(a.control_port, 9999);
    }

    #[test]
    fn ipv6_with_port() {
        let a = parse(&["[::1]:5000"]).unwrap().unwrap();
        assert_eq!(a.host, "::1");
        assert_eq!(a.control_port, 5000);
    }

    #[test]
    fn options_parse() {
        let a = parse(&["mac.local", "--fps", "30", "--codec", "hevc", "--token", "x"])
            .unwrap()
            .unwrap();
        assert_eq!(a.fps, 30);
        assert_eq!(a.codec, Codec::Hevc);
        assert_eq!(a.token.as_deref(), Some("x"));
    }

    #[test]
    fn mac_parses() {
        assert_eq!(
            parse_mac("aa:bb:cc:dd:ee:ff").unwrap(),
            [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]
        );
    }

    #[test]
    fn help_and_missing_host() {
        assert!(parse(&["--help"]).unwrap().is_none());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn rejects_unknown_flag() {
        assert!(parse(&["host", "--nope"]).is_err());
    }
}
