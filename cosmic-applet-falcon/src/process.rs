use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FalconProc {
    pub pid: u32,
    pub comm: String,
}

pub fn is_falcon_comm(comm: &str) -> bool {
    // /proc/<pid>/comm is truncated to 15 bytes, so "falcon-sensor-bpf"
    // shows up as "falcon-sensor-b" — match on the prefix.
    comm == "falcond" || comm.starts_with("falcon-sensor")
}

pub fn scan(proc_root: &Path) -> Vec<FalconProc> {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut found: Vec<FalconProc> = entries
        .flatten()
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let comm = std::fs::read_to_string(entry.path().join("comm")).ok()?;
            let comm = comm.trim();
            is_falcon_comm(comm).then(|| FalconProc {
                pid,
                comm: comm.to_owned(),
            })
        })
        .collect();
    found.sort_by_key(|p| p.pid);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comm_matching() {
        assert!(is_falcon_comm("falcond"));
        assert!(is_falcon_comm("falcon-sensor-b"));
        assert!(is_falcon_comm("falcon-sensor"));
        assert!(!is_falcon_comm("falcon"));
        assert!(!is_falcon_comm("bash"));
        assert!(!is_falcon_comm(""));
    }

    #[test]
    fn scan_finds_falcon_pids_sorted() {
        let root = tempfile::tempdir().expect("tempdir");
        let mk = |pid: &str, comm: &str| {
            let dir = root.path().join(pid);
            std::fs::create_dir(&dir).expect("proc dir");
            std::fs::write(dir.join("comm"), format!("{comm}\n")).expect("comm");
        };
        mk("900", "falcon-sensor-b");
        mk("100", "falcond");
        mk("200", "bash");
        std::fs::create_dir(root.path().join("self")).expect("non-pid dir");
        std::fs::write(root.path().join("uptime"), "42").expect("plain file");

        let found = scan(root.path());
        assert_eq!(
            found,
            vec![
                FalconProc {
                    pid: 100,
                    comm: "falcond".to_owned()
                },
                FalconProc {
                    pid: 900,
                    comm: "falcon-sensor-b".to_owned()
                },
            ]
        );
    }

    #[test]
    fn scan_missing_root_is_empty() {
        assert!(scan(Path::new("/nonexistent-proc-root")).is_empty());
    }
}
