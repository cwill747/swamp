use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: u32,
    pub cpu: f32,
    pub rss_kb: u64,
    pub time_secs: u64,
    pub comm: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub session_pid: Option<u32>,
    pub procs: Vec<ProcInfo>,
    pub total_cpu: f32,
    pub total_rss_bytes: u64,
    pub total_user_time_secs: u64,
    pub load1: f32,
    pub load5: f32,
    pub load15: f32,
    pub mem_total_bytes: u64,
    pub mem_used_bytes: u64,
}

pub fn sample(session_name: &str, cached_roots: &mut Vec<u32>) -> Result<Snapshot> {
    let procs = list_processes()?;
    let by_pid: HashMap<u32, usize> = procs.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();

    // Resolve session roots: zellij processes whose argv references this session.
    // Cache them; re-resolve if any cached pid disappears or the cache is empty.
    let cache_valid = !cached_roots.is_empty()
        && cached_roots.iter().all(|p| by_pid.contains_key(p));
    if !cache_valid {
        *cached_roots = find_session_roots(session_name, &procs);
    }

    let mut snap = Snapshot::default();
    snap.session_pid = cached_roots.first().copied();

    if !cached_roots.is_empty() {
        let mut seen = HashSet::new();
        for &root in cached_roots.iter() {
            for i in descendants(root, &procs, &by_pid) {
                if !seen.insert(procs[i].pid) {
                    continue;
                }
                let p = &procs[i];
                snap.total_cpu += p.cpu;
                snap.total_rss_bytes += p.rss_kb * 1024;
                snap.total_user_time_secs += p.time_secs;
                snap.procs.push(p.clone());
            }
        }
        snap.procs
            .sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal));
    }

    let (l1, l5, l15) = load_avg();
    snap.load1 = l1;
    snap.load5 = l5;
    snap.load15 = l15;

    let (total, used) = mem_stats();
    snap.mem_total_bytes = total;
    snap.mem_used_bytes = used;

    Ok(snap)
}

fn list_processes() -> Result<Vec<ProcInfo>> {
    let out = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pcpu=,rss=,time=,comm="])
        .output()
        .context("ps -A")?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut v = Vec::new();
    for line in s.lines() {
        let mut rest = line.trim_start();
        let mut take = || -> Option<&str> {
            rest = rest.trim_start();
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            if end == 0 {
                return None;
            }
            let tok = &rest[..end];
            rest = &rest[end..];
            Some(tok)
        };
        let Some(pid) = take().and_then(|x| x.parse().ok()) else { continue };
        let Some(ppid) = take().and_then(|x| x.parse().ok()) else { continue };
        let Some(cpu) = take().and_then(|x| x.parse().ok()) else { continue };
        let Some(rss) = take().and_then(|x| x.parse().ok()) else { continue };
        let time_str = take().unwrap_or("0:00").to_string();
        let comm = rest.trim().to_string();
        v.push(ProcInfo {
            pid,
            ppid,
            cpu,
            rss_kb: rss,
            time_secs: parse_time(&time_str),
            comm,
        });
    }
    Ok(v)
}

fn parse_time(s: &str) -> u64 {
    // Possible formats: "MM:SS.ss", "HH:MM:SS", "D-HH:MM:SS".
    let s = s.trim();
    let (days, rest) = if let Some((d, r)) = s.split_once('-') {
        (d.parse::<u64>().unwrap_or(0), r)
    } else {
        (0, s)
    };
    let parts: Vec<&str> = rest.split(':').collect();
    let int_part = |x: &str| {
        x.split('.')
            .next()
            .unwrap_or("0")
            .parse::<u64>()
            .unwrap_or(0)
    };
    let (h, m, sec) = match parts.as_slice() {
        [m, s] => (0u64, int_part(m), int_part(s)),
        [h, m, s] => (int_part(h), int_part(m), int_part(s)),
        _ => (0, 0, 0),
    };
    days * 86400 + h * 3600 + m * 60 + sec
}

fn find_session_roots(session_name: &str, procs: &[ProcInfo]) -> Vec<u32> {
    // Identify all zellij processes (client + server) whose argv references this
    // session by looking up full command lines via `ps -A -o pid=,args=`.  The
    // server owns the pane shells; the client owns its own subtree.  Walking
    // descendants of both — deduped — captures every process in the session.
    let Ok(out) = Command::new("ps").args(["-A", "-o", "pid=,args="]).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut roots = Vec::new();
    let live: HashMap<u32, ()> = procs.iter().map(|p| (p.pid, ())).collect();
    for line in text.lines() {
        let line = line.trim_start();
        let Some((pid_str, args)) = line.split_once(char::is_whitespace) else { continue };
        if !args.contains("zellij") {
            continue;
        }
        // Word-bounded match for the session name to avoid false hits (e.g. a
        // session name that appears as a substring of a path).
        if !contains_token(args, session_name) {
            continue;
        }
        let Ok(pid) = pid_str.parse::<u32>() else { continue };
        if live.contains_key(&pid) {
            roots.push(pid);
        }
    }
    roots
}

fn contains_token(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let n = needle.as_bytes();
    let is_boundary = |c: u8| !c.is_ascii_alphanumeric() && c != b'_' && c != b'-';
    let mut i = 0;
    while let Some(pos) = haystack[i..].find(needle) {
        let abs = i + pos;
        let before_ok = abs == 0 || is_boundary(bytes[abs - 1]);
        let after = abs + n.len();
        let after_ok = after == bytes.len() || is_boundary(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
        i = abs + 1;
    }
    false
}

fn descendants(
    root: u32,
    procs: &[ProcInfo],
    by_pid: &HashMap<u32, usize>,
) -> Vec<usize> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(&i) = by_pid.get(&pid) {
            out.push(i);
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    out
}

fn load_avg() -> (f32, f32, f32) {
    if let Ok(out) = Command::new("sysctl").args(["-n", "vm.loadavg"]).output() {
        let s = String::from_utf8_lossy(&out.stdout);
        let nums: Vec<f32> = s
            .split_whitespace()
            .filter_map(|w| w.trim_matches(|c: char| c == '{' || c == '}').parse().ok())
            .collect();
        if nums.len() >= 3 {
            return (nums[0], nums[1], nums[2]);
        }
    }
    if let Ok(s) = std::fs::read_to_string("/proc/loadavg") {
        let nums: Vec<f32> = s
            .split_whitespace()
            .take(3)
            .filter_map(|w| w.parse().ok())
            .collect();
        if nums.len() == 3 {
            return (nums[0], nums[1], nums[2]);
        }
    }
    (0.0, 0.0, 0.0)
}

fn mem_stats() -> (u64, u64) {
    // macOS path.
    if let Ok(out) = Command::new("sysctl").args(["-n", "hw.memsize"]).output() {
        let s = String::from_utf8_lossy(&out.stdout);
        if let Ok(total) = s.trim().parse::<u64>() {
            if total > 0 {
                if let Ok(vm) = Command::new("vm_stat").output() {
                    let vm = String::from_utf8_lossy(&vm.stdout);
                    let mut page_size: u64 = 4096;
                    let mut free: u64 = 0;
                    let mut speculative: u64 = 0;
                    for line in vm.lines() {
                        if let Some(rest) =
                            line.strip_prefix("Mach Virtual Memory Statistics: (page size of ")
                        {
                            if let Some(num) = rest.split_whitespace().next() {
                                page_size = num.parse().unwrap_or(4096);
                            }
                        } else if let Some(rest) = line.strip_prefix("Pages free:") {
                            free = parse_pages(rest);
                        } else if let Some(rest) = line.strip_prefix("Pages speculative:") {
                            speculative = parse_pages(rest);
                        }
                    }
                    let free_bytes = (free + speculative) * page_size;
                    return (total, total.saturating_sub(free_bytes));
                }
                return (total, 0);
            }
        }
    }
    // Linux fallback.
    if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
        let mut total_kb = 0u64;
        let mut avail_kb = 0u64;
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("MemTotal:") {
                total_kb = v
                    .split_whitespace()
                    .next()
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("MemAvailable:") {
                avail_kb = v
                    .split_whitespace()
                    .next()
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0);
            }
        }
        let total = total_kb * 1024;
        let avail = avail_kb * 1024;
        return (total, total.saturating_sub(avail));
    }
    (0, 0)
}

fn parse_pages(s: &str) -> u64 {
    s.trim().trim_end_matches('.').parse().unwrap_or(0)
}

pub fn fmt_bytes(b: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = b as f64;
    if b >= GB {
        format!("{:.1}G", b / GB)
    } else if b >= MB {
        format!("{:.0}M", b / MB)
    } else if b >= KB {
        format!("{:.0}K", b / KB)
    } else {
        format!("{b:.0}B")
    }
}

pub fn fmt_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}
