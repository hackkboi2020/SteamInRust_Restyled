//! LAN discovery: find who on the same network is hosting a game.
//!
//! Hosts broadcast a small announcement over UDP every few seconds; everyone
//! listens and keeps a list of hosts seen recently. This only finds people.
//! The game's own networking does the actual playing, so joining means
//! starting the game with the host's address filled into the launch options
//! (see [`fill_template`]).
//!
//! Everything received is untrusted input from the network: packets are size
//! limited, text is stripped of control characters and truncated, and the
//! table of known hosts has a fixed maximum size.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, UdpSocket};
use std::time::{Duration, Instant};

pub const PORT: u16 = 47620;
const MAGIC: &str = "SIRLAN1";
const MAX_PACKET: usize = 512;
const MAX_TEXT: usize = 48;
const MAX_HOSTS: usize = 256;

/// How long a host stays listed without a new announcement.
pub const TTL: Duration = Duration::from_secs(10);
/// How often a host should announce itself.
pub const INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announce {
    /// Random per run, so our own broadcasts can be ignored.
    pub instance: u64,
    /// Who is hosting.
    pub nick: String,
    /// The game being hosted.
    pub game: String,
    /// The game's own port, for joining.
    pub port: u16,
    pub players: u8,
    pub max_players: u8,
}

/// No control characters (tabs and newlines would break the format), at most
/// [`MAX_TEXT`] characters, no surrounding spaces.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect::<String>().trim().to_string()
}

pub fn encode(a: &Announce) -> String {
    format!("{MAGIC}\t{:x}\t{}\t{}\t{}\t{}\t{}", a.instance, clean(&a.nick), clean(&a.game), a.port, a.players, a.max_players)
}

/// `None` for anything that isn't a well-formed announcement.
pub fn decode(text: &str) -> Option<Announce> {
    let mut parts = text.split('\t');
    if parts.next()? != MAGIC {
        return None;
    }
    let instance = u64::from_str_radix(parts.next()?, 16).ok()?;
    let nick = clean(parts.next()?);
    let game = clean(parts.next()?);
    let port: u16 = parts.next()?.parse().ok()?;
    let players: u8 = parts.next()?.parse().ok()?;
    let max_players: u8 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || port == 0 || game.is_empty() {
        return None;
    }
    Some(Announce { instance, nick, game, port, players, max_players })
}

#[derive(Debug, Clone)]
pub struct Host {
    pub ip: IpAddr,
    pub announce: Announce,
    pub seen: Instant,
}

/// Hosts heard recently.
#[derive(Default)]
pub struct Peers {
    hosts: HashMap<(IpAddr, u64), Host>,
}

impl Peers {
    pub fn update(&mut self, ip: IpAddr, announce: Announce, now: Instant) {
        let key = (ip, announce.instance);
        if !self.hosts.contains_key(&key) && self.hosts.len() >= MAX_HOSTS {
            self.prune(now);
            if self.hosts.len() >= MAX_HOSTS {
                return; // a flood of fake hosts must not grow memory
            }
        }
        self.hosts.insert(key, Host { ip, announce, seen: now });
    }

    pub fn prune(&mut self, now: Instant) {
        self.hosts.retain(|_, h| now.saturating_duration_since(h.seen) <= TTL);
    }

    /// Hosts still fresh, sorted by game then host name.
    pub fn live(&self, now: Instant) -> Vec<&Host> {
        let mut v: Vec<&Host> = self.hosts.values().filter(|h| now.saturating_duration_since(h.seen) <= TTL).collect();
        v.sort_by(|a, b| (&a.announce.game, &a.announce.nick, a.ip).cmp(&(&b.announce.game, &b.announce.nick, b.ip)));
        v
    }
}

/// Launch options for joining: `{ip}` and `{port}` are replaced. Only these
/// two values (an address parsed from the packet's source and a number) are
/// ever inserted; text from the network is never put on a command line.
pub fn fill_template(template: &str, ip: IpAddr, port: u16) -> String {
    template.replace("{ip}", &ip.to_string()).replace("{port}", &port.to_string())
}

/// A UDP socket that sends and receives announcements.
pub struct Beacon {
    sock: UdpSocket,
}

impl Beacon {
    pub fn bind() -> io::Result<Self> {
        let sock = UdpSocket::bind(("0.0.0.0", PORT))?;
        sock.set_broadcast(true)?;
        sock.set_nonblocking(true)?;
        Ok(Self { sock })
    }

    pub fn announce(&self, a: &Announce) -> io::Result<()> {
        self.sock.send_to(encode(a).as_bytes(), ("255.255.255.255", PORT)).map(|_| ())
    }

    /// Read everything waiting, add valid announcements from others to
    /// `peers`. Never blocks.
    pub fn poll(&self, own_instance: u64, peers: &mut Peers, now: Instant) {
        let mut buf = [0u8; MAX_PACKET];
        loop {
            match self.sock.recv_from(&mut buf) {
                // A full buffer may have been cut off: don't trust it.
                Ok((n, _)) if n >= MAX_PACKET => continue,
                Ok((n, from)) => {
                    if let Some(a) = std::str::from_utf8(&buf[..n]).ok().and_then(decode) {
                        if a.instance != own_instance {
                            peers.update(from.ip(), a, now);
                        }
                    }
                }
                Err(_) => break, // WouldBlock: nothing more waiting
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn sample() -> Announce {
        Announce { instance: 0xABCD, nick: "Sam".into(), game: "Minecraft".into(), port: 25565, players: 2, max_players: 8 }
    }

    #[test]
    fn encode_then_decode_round_trips() {
        assert_eq!(decode(&encode(&sample())), Some(sample()));
    }

    #[test]
    fn tabs_and_newlines_cannot_break_the_format() {
        let mut a = sample();
        a.nick = "evil\tname\nhere".into();
        let back = decode(&encode(&a)).expect("still valid");
        assert_eq!(back.nick, "evilnamehere");
        assert_eq!(back.game, "Minecraft");
    }

    #[test]
    fn long_text_is_truncated() {
        let mut a = sample();
        a.game = "x".repeat(500);
        assert_eq!(decode(&encode(&a)).unwrap().game.chars().count(), MAX_TEXT);
    }

    #[test]
    fn junk_is_rejected() {
        for bad in ["", "hello", "SIRLAN1", "SIRLAN2\t1\ta\tb\t1\t1\t1", "SIRLAN1\tzz\ta\tb\t1\t1\t1", "SIRLAN1\t1\ta\tb\t0\t1\t1", "SIRLAN1\t1\ta\t\t1\t1\t1", "SIRLAN1\t1\ta\tb\t99999\t1\t1", "SIRLAN1\t1\ta\tb\t1\t1\t1\textra"] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn hosts_expire_and_are_sorted() {
        let t0 = Instant::now();
        let ip = |n| IpAddr::V4(Ipv4Addr::new(192, 168, 0, n));
        let mut peers = Peers::default();
        peers.update(ip(5), Announce { game: "Zed".into(), ..sample() }, t0);
        peers.update(ip(6), Announce { instance: 2, game: "Alpha".into(), ..sample() }, t0);
        let games: Vec<_> = peers.live(t0).iter().map(|h| h.announce.game.clone()).collect();
        assert_eq!(games, ["Alpha", "Zed"]);
        assert!(peers.live(t0 + TTL + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn the_host_table_cannot_be_flooded() {
        let t0 = Instant::now();
        let mut peers = Peers::default();
        for i in 0..(MAX_HOSTS as u64 + 100) {
            peers.update(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), Announce { instance: i, ..sample() }, t0);
        }
        assert_eq!(peers.live(t0).len(), MAX_HOSTS);
    }

    #[test]
    fn the_join_template_only_inserts_address_and_port() {
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
        assert_eq!(fill_template("--connect {ip}:{port}", ip, 25565), "--connect 192.168.1.20:25565");
        assert_eq!(fill_template("-nojoin", ip, 1), "-nojoin");
    }
}
