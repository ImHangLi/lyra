//! Machine-wide process facts for `lyra ps`: one `ps` call for memory and one `lsof` call for
//! TCP listeners, aggregated per process group. Parsing and flag rules are pure so they
//! can be tested without processes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::process::{Command, Stdio};

/// One row of `ps -axo pid=,pgid=,rss=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcRow {
    pub pid: u32,
    pub pgid: u32,
    pub rss_kib: u64,
}

/// Parses `ps -axo pid=,pgid=,rss=` output; malformed lines are skipped.
pub fn parse_ps(text: &str) -> Vec<ProcRow> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let row = ProcRow {
                pid: f.next()?.parse().ok()?,
                pgid: f.next()?.parse().ok()?,
                rss_kib: f.next()?.parse().ok()?,
            };
            f.next().is_none().then_some(row)
        })
        .collect()
}

/// A TCP listener: the owning process and the port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Listener {
    pub pid: u32,
    pub port: u16,
}

/// Parses `lsof -nP -iTCP -sTCP:LISTEN -F pn` field output (`p<pid>`, `f<fd>`, `n<addr>:<port>`).
pub fn parse_lsof(text: &str) -> Vec<Listener> {
    let mut out = Vec::new();
    let mut pid = None;
    for line in text.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => pid = value.parse().ok(),
            "n" => {
                if let (Some(pid), Some(port)) = (pid, port_after(value, ':')) {
                    out.push(Listener { pid, port });
                }
            }
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

fn port_after(addr: &str, sep: char) -> Option<u16> {
    addr.rsplit(sep).next()?.parse().ok()
}

/// Memory and ports per process group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupFacts {
    pub processes: u32,
    pub rss_kib: u64,
    pub ports: BTreeSet<u16>,
}

/// A machine snapshot: every process by PID, and facts per process group.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub rss_by_pid: HashMap<u32, u64>,
    pub groups: HashMap<u32, GroupFacts>,
}

impl Snapshot {
    pub fn build(procs: &[ProcRow], listeners: &[Listener]) -> Self {
        let mut s = Self::default();
        let mut pgid_of = HashMap::with_capacity(procs.len());
        for p in procs {
            s.rss_by_pid.insert(p.pid, p.rss_kib);
            pgid_of.insert(p.pid, p.pgid);
            let g = s.groups.entry(p.pgid).or_default();
            g.processes += 1;
            g.rss_kib += p.rss_kib;
        }
        for l in listeners {
            if let Some(g) = pgid_of.get(&l.pid).and_then(|pg| s.groups.get_mut(pg)) {
                g.ports.insert(l.port);
            }
        }
        s
    }

    pub fn group(&self, pgid: u32) -> Option<&GroupFacts> {
        self.groups.get(&pgid)
    }

    /// Runs one `ps` and one `lsof` call side by side. `lsof` costs about 0.3 s on macOS
    /// whatever it lists, so it starts first and `ps` runs while it works.
    pub fn collect() -> Self {
        let lsof = Command::new("/usr/sbin/lsof")
            .args(["-b", "-w", "-nP", "-iTCP", "-sTCP:LISTEN", "-F", "pn"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let ps = Command::new("/bin/ps")
            .args(["-axo", "pid=,pgid=,rss="])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|o| parse_ps(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default();
        let listeners = lsof
            .and_then(|c| c.wait_with_output())
            .map(|o| parse_lsof(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default();
        Self::build(&ps, &listeners)
    }
}

/// What a flag rule needs to know about one workspace.
#[derive(Debug, Clone, Default)]
pub struct FlagInput<'a> {
    pub name: &'a str,
    pub other_build: Option<&'a str>,
    pub unreachable: bool,
    /// Background session expiry, in milliseconds from now.
    pub background_expires_in_ms: Option<i64>,
    pub runs: Vec<FlagRun<'a>>,
}

#[derive(Debug, Clone)]
pub struct FlagRun<'a> {
    pub label: &'a str,
    pub run_id: &'a str,
    pub running: bool,
    pub age_ms: i64,
    /// The recorded group leader; `None` when the host has no ledger entry for it.
    pub pgid: Option<u32>,
    /// True when `ps` shows a process in that group.
    pub group_alive: bool,
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Flag {
    pub code: &'static str,
    pub workspace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub message: String,
}

pub const EXPIRY_SOON_MS: i64 = 10 * 60 * 1000;
/// A new run's ledger entry is written just after spawn; younger runs are not judged.
const LEDGER_GRACE_MS: i64 = 5_000;

pub fn human_duration(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        3600..86_400 => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d{:02}h", s / 86_400, (s % 86_400) / 3600),
    }
}

/// The problems worth a human's attention, in a stable order.
pub fn flags(workspaces: &[FlagInput<'_>]) -> Vec<Flag> {
    let mut out = Vec::new();
    let mut by_port: BTreeMap<u16, Vec<String>> = BTreeMap::new();
    for w in workspaces {
        let flag = |code, run_id: Option<&str>, message: String| Flag {
            code,
            workspace: w.name.to_owned(),
            run_id: run_id.map(str::to_owned),
            message,
        };
        if let Some(version) = w.other_build {
            out.push(flag(
                "OTHER_BUILD",
                None,
                format!(
                    "{}: host from another build (lyra {version}); `lyra down --all` stops it",
                    w.name
                ),
            ));
        }
        if w.unreachable {
            out.push(flag(
                "UNREACHABLE",
                None,
                format!("{}: the host did not answer in time", w.name),
            ));
        }
        if let Some(left) = w.background_expires_in_ms.filter(|l| *l < EXPIRY_SOON_MS) {
            out.push(flag(
                "EXPIRES_SOON",
                None,
                format!(
                    "{}: background session ends in {}; `lyra keep` extends it",
                    w.name,
                    human_duration(left)
                ),
            ));
        }
        for r in &w.runs {
            for p in &r.ports {
                by_port
                    .entry(*p)
                    .or_default()
                    .push(format!("{} {}", w.name, r.label));
            }
            if !r.running || r.age_ms < LEDGER_GRACE_MS {
                continue;
            }
            let gone = match r.pgid {
                Some(pg) if !r.group_alive => Some(format!("process group {pg} is gone")),
                None => Some("no process group is recorded".to_owned()),
                Some(_) => None,
            };
            if let Some(why) = gone {
                out.push(flag(
                    "GROUP_GONE",
                    Some(r.run_id),
                    format!("{} {}: the host says running, but {why}", w.name, r.label),
                ));
            }
        }
    }
    for (port, owners) in by_port {
        if owners.len() > 1 {
            out.push(Flag {
                code: "PORT_SHARED",
                workspace: owners[0].split(' ').next().unwrap_or("").to_owned(),
                run_id: None,
                message: format!("port {port} is listened on by {}", owners.join(" and ")),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_rows_parse_and_bad_lines_are_skipped() {
        let rows = parse_ps("  101   101  2048\n 102 101 512\nbad line\n 7 7\n 1 1 1 1\n");
        assert_eq!(
            rows,
            [
                ProcRow {
                    pid: 101,
                    pgid: 101,
                    rss_kib: 2048
                },
                ProcRow {
                    pid: 102,
                    pgid: 101,
                    rss_kib: 512
                },
            ]
        );
    }

    #[test]
    fn lsof_fields_parse_ipv4_ipv6_and_dedupe() {
        let text =
            "p743\ng743\nf9\nn*:7000\nf10\nn*:7000\np1021\nf6\nn127.0.0.1:44950\nf7\nn[::1]:3000\n";
        assert_eq!(
            parse_lsof(text),
            [
                Listener {
                    pid: 743,
                    port: 7000
                },
                Listener {
                    pid: 1021,
                    port: 3000
                },
                Listener {
                    pid: 1021,
                    port: 44950
                },
            ]
        );
    }

    #[test]
    fn memory_and_ports_aggregate_per_process_group() {
        let procs = parse_ps("10 10 1000\n11 10 500\n12 10 250\n20 20 4000\n30 20 1\n");
        let listeners = [
            Listener {
                pid: 11,
                port: 3000,
            },
            Listener {
                pid: 12,
                port: 3001,
            },
            Listener {
                pid: 30,
                port: 8080,
            },
            Listener { pid: 99, port: 9 },
        ];
        let s = Snapshot::build(&procs, &listeners);
        let g = s.group(10).unwrap();
        assert_eq!((g.processes, g.rss_kib), (3, 1750));
        assert_eq!(g.ports.iter().copied().collect::<Vec<_>>(), [3000, 3001]);
        assert_eq!(s.group(20).unwrap().rss_kib, 4001);
        assert_eq!(s.rss_by_pid.get(&20), Some(&4000));
        assert!(s.group(99).is_none());
    }

    fn run<'a>(label: &'a str, pgid: Option<u32>, alive: bool, ports: &[u16]) -> FlagRun<'a> {
        FlagRun {
            label,
            run_id: "r_1",
            running: true,
            age_ms: 60_000,
            pgid,
            group_alive: alive,
            ports: ports.to_vec(),
        }
    }

    #[test]
    fn flag_rules() {
        let a = FlagInput {
            name: "a",
            background_expires_in_ms: Some(5 * 60 * 1000),
            runs: vec![run("dev.web", Some(10), true, &[3000])],
            ..FlagInput::default()
        };
        let b = FlagInput {
            name: "b",
            background_expires_in_ms: Some(60 * 60 * 1000),
            runs: vec![
                run("dev.web", Some(20), true, &[3000]),
                run("dev.api", Some(30), false, &[]),
            ],
            ..FlagInput::default()
        };
        let old = FlagInput {
            name: "old",
            other_build: Some("0.1.1"),
            ..FlagInput::default()
        };
        let codes: Vec<_> = flags(&[a, b, old])
            .into_iter()
            .map(|f| (f.code, f.workspace))
            .collect();
        assert_eq!(
            codes,
            [
                ("EXPIRES_SOON", "a".to_owned()),
                ("GROUP_GONE", "b".to_owned()),
                ("OTHER_BUILD", "old".to_owned()),
                ("PORT_SHARED", "a".to_owned()),
            ]
        );
    }

    #[test]
    fn young_or_finished_runs_are_not_judged_gone() {
        let mut young = run("dev.web", None, false, &[]);
        young.age_ms = 1_000;
        let mut stopping = run("dev.api", Some(5), false, &[]);
        stopping.running = false;
        let w = FlagInput {
            name: "a",
            runs: vec![young, stopping],
            ..FlagInput::default()
        };
        assert!(flags(&[w]).is_empty());
    }

    #[test]
    fn durations_read_well() {
        assert_eq!(human_duration(42_000), "42s");
        assert_eq!(human_duration(125_000), "2m05s");
        assert_eq!(human_duration(7_260_000), "2h01m");
        assert_eq!(human_duration(-5), "0s");
    }
}
