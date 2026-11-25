//! Section + CSV parsing for the remote probe script (DESIGN §5.2/§5.4).
//!
//! The parser is deliberately forgiving about individual fields (a missing
//! value becomes `null`, which later turns into `unknown`), but strict about
//! structure: a missing `@@END`, `@@NOGPU`, or `@@GPU` section is an error.

use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawGpu {
    pub index: Option<u32>,
    pub uuid: Option<String>,
    pub name: Option<String>,
    pub memory_total_mb: Option<u64>,
    pub memory_used_mb: Option<u64>,
    pub memory_free_mb: Option<u64>,
    pub util_pct: Option<u32>,
    pub temp_c: Option<u32>,
    pub compute_mode: Option<String>,
    pub driver: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuSample {
    pub rows: Vec<RawGpu>,
    pub rc: Option<i32>,
    pub fallback: bool,
    pub noise: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawProcess {
    pub gpu_uuid: String,
    pub pid: Option<u32>,
    pub process_name: String,
    pub used_memory_mb: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub hostname: Option<String>,
    pub load1: Option<f64>,
    pub cpus: Option<u32>,
    pub nogpu: Option<u8>,
    pub saw_nogpu_section: bool,
    pub mig: bool,
    pub gpu_samples: Vec<GpuSample>,
    pub saw_gpu_section: bool,
    pub gpu_section_count: usize,
    pub saw_proc_section: bool,
    pub proc_rows: Vec<RawProcess>,
    pub proc_rc: Option<i32>,
    pub proc_noise: Vec<String>,
    /// pid → user from the `ps` section.
    pub ps: HashMap<u32, String>,
    pub ended: bool,
}

impl Report {
    /// Structural validation after the section scan.
    ///
    /// The real script (`src/probe/script.rs`) prints `@@NOGPU 1|2` and exits
    /// before `@@GPU/@@PROC/@@PS/@@END`, so a no-GPU host is a complete report
    /// without those sections (A26/PRS-03). Everything else must be whole.
    pub fn validate(&self) -> Result<(), String> {
        let Some(nogpu) = self.nogpu else {
            return Err(if self.saw_nogpu_section {
                "invalid @@NOGPU value".to_string()
            } else {
                "missing @@NOGPU section".to_string()
            });
        };
        if self.gpu_section_count > 1 {
            return Err("duplicate @@GPU section".to_string());
        }
        match nogpu {
            1 | 2 => return Ok(()),
            0 => {}
            other => return Err(format!("invalid @@NOGPU value: {other}")),
        }
        if !self.ended {
            return Err("output truncated: missing @@END".to_string());
        }
        if !self.saw_gpu_section {
            return Err("missing @@GPU section".to_string());
        }
        Ok(())
    }

    /// First GPU sample whose return code is not zero, with its error text.
    pub fn gpu_failure(&self) -> Option<(i32, String)> {
        for sample in &self.gpu_samples {
            match sample.rc {
                Some(0) => {}
                Some(rc) => {
                    let message = if sample.noise.is_empty() {
                        format!("nvidia-smi exited with code {rc}")
                    } else {
                        sample.noise.join("; ")
                    };
                    return Some((rc, message));
                }
                None => {
                    return Some((-1, "nvidia-smi return code missing".to_string()));
                }
            }
        }
        None
    }

    pub fn gpu_rows(&self) -> Vec<RawGpu> {
        merge_samples(&self.gpu_samples)
    }
}

enum Section {
    None,
    Host,
    NoGpu,
    Mig,
    Gpu,
    Proc,
    Ps,
    End,
}

pub fn parse(text: &str) -> Report {
    let mut report = Report::default();
    let mut section = Section::None;
    let mut host_lines: Vec<String> = Vec::new();
    let mut fallback_mode = false;

    for line in text.lines() {
        if let Some(marker) = line.strip_prefix("@@") {
            if let Some((key, value)) = marker.split_once('=') {
                match key {
                    "GPU_RC" => {
                        if report.gpu_samples.is_empty() {
                            report.gpu_samples.push(GpuSample {
                                fallback: fallback_mode,
                                ..GpuSample::default()
                            });
                        }
                        if let Some(sample) = report.gpu_samples.last_mut() {
                            sample.rc = value.trim().parse().ok();
                        }
                    }
                    "PROC_RC" => {
                        report.proc_rc = value.trim().parse().ok();
                    }
                    "FALLBACK" if value.trim() == "1" => {
                        fallback_mode = true;
                        for sample in report.gpu_samples.iter_mut() {
                            if sample.rc.is_none() {
                                sample.fallback = true;
                            }
                        }
                    }
                    _ => {}
                }
                continue;
            }
            section = match marker.trim() {
                "HOST" => Section::Host,
                "NOGPU" => {
                    report.saw_nogpu_section = true;
                    Section::NoGpu
                }
                "MIG" => Section::Mig,
                "GPU" => {
                    report.saw_gpu_section = true;
                    report.gpu_section_count += 1;
                    Section::Gpu
                }
                "PROC" => {
                    report.saw_proc_section = true;
                    Section::Proc
                }
                "PS" => Section::Ps,
                "END" => {
                    report.ended = true;
                    Section::End
                }
                _ => section,
            };
            continue;
        }

        match section {
            Section::Host => host_lines.push(line.to_string()),
            Section::NoGpu => {
                if report.nogpu.is_none() {
                    report.nogpu = line.trim().parse().ok();
                }
            }
            Section::Mig => {
                if line.trim() == "1" {
                    report.mig = true;
                }
            }
            Section::Gpu => {
                let need_new = report
                    .gpu_samples
                    .last()
                    .is_none_or(|sample| sample.rc.is_some());
                if need_new {
                    report.gpu_samples.push(GpuSample {
                        fallback: fallback_mode,
                        ..GpuSample::default()
                    });
                }
                if line.trim().is_empty() {
                    continue;
                }
                let sample = report
                    .gpu_samples
                    .last_mut()
                    .expect("a GPU sample was just ensured");
                match parse_gpu_row(line, sample.fallback) {
                    Some(row) => sample.rows.push(row),
                    None => sample.noise.push(line.trim().to_string()),
                }
            }
            Section::Proc => {
                if line.trim().is_empty() {
                    continue;
                }
                match parse_process_row(line) {
                    Some(row) => report.proc_rows.push(row),
                    None => report.proc_noise.push(line.trim().to_string()),
                }
            }
            Section::Ps => {
                let mut parts = line.split_whitespace();
                if let (Some(pid), Some(user)) = (parts.next(), parts.next())
                    && let Ok(pid) = pid.parse::<u32>()
                {
                    report.ps.insert(pid, user.to_string());
                }
            }
            Section::None | Section::End => {}
        }
    }

    fill_host_facts(&mut report, &host_lines);
    report
}

fn fill_host_facts(report: &mut Report, lines: &[String]) {
    for line in lines {
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = text.split_whitespace().collect();
        if tokens.len() >= 3 && tokens[0].parse::<f64>().is_ok() && report.load1.is_none() {
            report.load1 = tokens[0].parse().ok();
            continue;
        }
        if report.hostname.is_none() {
            report.hostname = Some(text.to_string());
            continue;
        }
        if tokens.len() == 1
            && report.cpus.is_none()
            && let Ok(cpus) = tokens[0].parse::<u32>()
        {
            report.cpus = Some(cpus);
        }
    }
}

/// Parse one GPU CSV row. `nounits` keeps suffixes like `MiB`/`%` off modern
/// drivers, but the parser strips them anyway because older ones emit them.
pub fn parse_gpu_row(line: &str, fallback: bool) -> Option<RawGpu> {
    let parts: Vec<&str> = line.split(',').map(str::trim).collect();
    let n = parts.len();
    if fallback {
        if n < 7 {
            return None;
        }
        let name_end = n - 4;
        Some(RawGpu {
            index: parse_u32(parts[0]),
            uuid: parse_text(parts[1]),
            name: join_name(&parts[2..name_end]),
            memory_total_mb: parse_u64(parts[n - 4]),
            memory_used_mb: parse_u64(parts[n - 3]),
            memory_free_mb: parse_u64(parts[n - 2]),
            util_pct: parse_u32(parts[n - 1]),
            temp_c: None,
            compute_mode: None,
            driver: None,
        })
    } else {
        if n < 10 {
            return None;
        }
        let name_end = n - 7;
        Some(RawGpu {
            index: parse_u32(parts[0]),
            uuid: parse_text(parts[1]),
            name: join_name(&parts[2..name_end]),
            memory_total_mb: parse_u64(parts[n - 7]),
            memory_used_mb: parse_u64(parts[n - 6]),
            memory_free_mb: parse_u64(parts[n - 5]),
            util_pct: parse_u32(parts[n - 4]),
            temp_c: parse_u32(parts[n - 3]),
            compute_mode: parse_text(parts[n - 2]),
            driver: parse_text(parts[n - 1]),
        })
    }
}

fn parse_process_row(line: &str) -> Option<RawProcess> {
    let parts = split_csv(line);
    if parts.len() < 2 {
        return None;
    }
    let gpu_uuid = parse_text(&parts[0])?;
    let pid = parse_u32(&parts[1]);
    let (process_name, used_memory_mb) = if parts.len() >= 3 {
        (
            // Quoted names arrive as one field and are kept verbatim; an
            // unquoted name with commas is rejoined with ", " as before.
            parts[2..parts.len() - 1].join(", ").trim().to_string(),
            parse_u64(&parts[parts.len() - 1]),
        )
    } else {
        (String::new(), None)
    };
    Some(RawProcess {
        gpu_uuid,
        pid,
        process_name,
        used_memory_mb,
    })
}

/// Split one CSV line, honouring double quotes: `"a,b"` stays one field and
/// `""` inside a quoted field is a literal quote (nvidia-smi quotes process
/// names that contain commas).
fn split_csv(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        if chars.peek() == Some(&'"') {
            chars.next();
            let mut field = String::new();
            while let Some(c) = chars.next() {
                if c == '"' {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        break;
                    }
                } else {
                    field.push(c);
                }
            }
            // Anything between the closing quote and the comma is kept so a
            // malformed row still shows its text instead of dropping it.
            let mut tail = String::new();
            while let Some(&c) = chars.peek() {
                if c == ',' {
                    break;
                }
                tail.push(c);
                chars.next();
            }
            if !tail.trim().is_empty() {
                field.push_str(tail.trim_end());
            }
            fields.push(field);
        } else {
            let mut field = String::new();
            while let Some(&c) = chars.peek() {
                if c == ',' {
                    break;
                }
                field.push(c);
                chars.next();
            }
            fields.push(field.trim().to_string());
        }
        match chars.next() {
            Some(',') => continue,
            _ => break,
        }
    }
    fields
}

fn join_name(parts: &[&str]) -> Option<String> {
    let joined = parts.join(", ");
    let trimmed = joined.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_text(value: &str) -> Option<String> {
    let text = value.trim().trim_matches('"').trim();
    if text.is_empty()
        || text.eq_ignore_ascii_case("[N/A]")
        || text.to_ascii_lowercase().starts_with("[not supported")
    {
        None
    } else {
        Some(text.to_string())
    }
}

pub fn parse_u64(value: &str) -> Option<u64> {
    let text = value.trim();
    if text.is_empty() || text.starts_with('[') {
        return None;
    }
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

pub fn parse_u32(value: &str) -> Option<u32> {
    parse_u64(value).and_then(|v| u32::try_from(v).ok())
}

/// Merge samples taking the worst reading per GPU: max used memory, max
/// utilization, min free memory. Order follows first appearance.
pub fn merge_samples(samples: &[GpuSample]) -> Vec<RawGpu> {
    let mut merged: Vec<RawGpu> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    for sample in samples {
        for row in &sample.rows {
            let key = match (&row.uuid, row.index) {
                (Some(uuid), _) => format!("uuid:{uuid}"),
                (None, Some(index)) => format!("index:{index}"),
                (None, None) => continue,
            };
            if let Some(&pos) = positions.get(&key) {
                merge_row(&mut merged[pos], row);
            } else {
                positions.insert(key, merged.len());
                merged.push(row.clone());
            }
        }
    }
    merged
}

fn merge_row(target: &mut RawGpu, other: &RawGpu) {
    target.memory_used_mb = max_opt(target.memory_used_mb, other.memory_used_mb);
    target.util_pct = max_opt(target.util_pct, other.util_pct);
    target.memory_free_mb = min_opt(target.memory_free_mb, other.memory_free_mb);
    fill(&mut target.index, &other.index);
    fill(&mut target.uuid, &other.uuid);
    fill(&mut target.name, &other.name);
    fill(&mut target.memory_total_mb, &other.memory_total_mb);
    fill(&mut target.temp_c, &other.temp_c);
    fill(&mut target.compute_mode, &other.compute_mode);
    fill(&mut target.driver, &other.driver);
}

fn fill<T: Clone>(target: &mut Option<T>, other: &Option<T>) {
    if target.is_none() {
        target.clone_from(other);
    }
}

fn max_opt<T: Ord + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

fn min_opt<T: Ord + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
@@HOST
fake-host
0.42 0.30 0.20 1/100 12345
64
@@NOGPU
0
@@MIG
0
@@GPU
0, GPU-aaaa, NVIDIA RTX 4090, 24564, 1, 24563, 0, 33, Default, 570.86.15
1, GPU-bbbb, NVIDIA RTX 4090, 24564, 12288, 12276, 87, 71, Default, 570.86.15
@@GPU_RC=0
@@PROC
GPU-bbbb, 12345, python train.py, 12000
@@PROC_RC=0
@@PS
12345 alice
@@END
";

    #[test]
    fn parses_full_report() {
        let report = parse(SAMPLE);
        report.validate().unwrap();
        assert_eq!(report.hostname.as_deref(), Some("fake-host"));
        assert_eq!(report.load1, Some(0.42));
        assert_eq!(report.cpus, Some(64));
        assert_eq!(report.nogpu, Some(0));
        assert!(!report.mig);
        let rows = report.gpu_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].uuid.as_deref(), Some("GPU-aaaa"));
        assert_eq!(rows[0].memory_total_mb, Some(24564));
        assert_eq!(rows[0].util_pct, Some(0));
        assert_eq!(rows[0].temp_c, Some(33));
        assert_eq!(rows[1].compute_mode.as_deref(), Some("Default"));
        assert_eq!(rows[1].driver.as_deref(), Some("570.86.15"));
        assert_eq!(report.proc_rows.len(), 1);
        assert_eq!(report.proc_rows[0].pid, Some(12345));
        assert_eq!(report.proc_rows[0].process_name, "python train.py");
        assert_eq!(report.proc_rows[0].used_memory_mb, Some(12000));
        assert_eq!(report.ps.get(&12345).map(String::as_str), Some("alice"));
        assert_eq!(report.proc_rc, Some(0));
    }

    #[test]
    fn gpu_names_with_commas_are_rejoined() {
        let line = "0, GPU-x, NVIDIA A100, 80GB, 81920, 0, 81920, 0, 30, Default, 570.0";
        let row = parse_gpu_row(line, false).unwrap();
        assert_eq!(row.name.as_deref(), Some("NVIDIA A100, 80GB"));
        assert_eq!(row.memory_total_mb, Some(81920));
        assert_eq!(row.driver.as_deref(), Some("570.0"));
    }

    #[test]
    fn units_and_na_are_handled() {
        let line =
            "0, GPU-x, T4, 15360 MiB, [N/A], 15100 MiB, 5 %, [Not Supported], Default, [N/A]";
        let row = parse_gpu_row(line, false).unwrap();
        assert_eq!(row.memory_total_mb, Some(15360));
        assert_eq!(row.memory_used_mb, None);
        assert_eq!(row.memory_free_mb, Some(15100));
        assert_eq!(row.util_pct, Some(5));
        assert_eq!(row.temp_c, None);
        assert_eq!(row.compute_mode.as_deref(), Some("Default"));
        assert_eq!(row.driver, None);
    }

    #[test]
    fn fallback_rows_have_no_tier_two_fields() {
        let line = "2, GPU-y, P100, 16384, 100, 16284, 3";
        let row = parse_gpu_row(line, true).unwrap();
        assert_eq!(row.name.as_deref(), Some("P100"));
        assert_eq!(row.memory_total_mb, Some(16384));
        assert_eq!(row.util_pct, Some(3));
        assert_eq!(row.temp_c, None);
        assert_eq!(row.compute_mode, None);
        assert_eq!(row.driver, None);
        assert!(parse_gpu_row(line, false).is_none());
    }

    #[test]
    fn process_names_with_commas_keep_their_memory_column() {
        let line = "GPU-x, 999, python -c foo, bar, 2048";
        let row = parse_process_row(line).unwrap();
        assert_eq!(row.process_name, "python -c foo, bar");
        assert_eq!(row.used_memory_mb, Some(2048));
        assert_eq!(row.pid, Some(999));
    }

    #[test]
    fn quoted_process_name_keeps_a_literal_comma() {
        // PRS-15: nvidia-smi quotes names containing commas.
        let line = "GPU-0000, 1234, \"python train.py --name a,b\", 100";
        let row = parse_process_row(line).unwrap();
        assert_eq!(row.process_name, "python train.py --name a,b");
        assert_eq!(row.used_memory_mb, Some(100));
        assert_eq!(row.pid, Some(1234));
        assert_eq!(row.gpu_uuid, "GPU-0000");
    }

    #[test]
    fn quoted_process_name_unescapes_doubled_quotes() {
        let line = "GPU-0000, 1234, \"python train.py --name \"\"a,b\"\"\", 100";
        let row = parse_process_row(line).unwrap();
        assert_eq!(row.process_name, "python train.py --name \"a,b\"");
        assert_eq!(row.used_memory_mb, Some(100));
    }

    #[test]
    fn unquoted_extra_commas_are_still_tolerated() {
        let parts = split_csv("GPU-x, 7, python -c foo, bar, 2048");
        assert_eq!(parts, ["GPU-x", "7", "python -c foo", "bar", "2048"]);
        let row = parse_process_row("GPU-x, 7, python -c foo, bar, 2048").unwrap();
        assert_eq!(row.process_name, "python -c foo, bar");
        assert_eq!(row.used_memory_mb, Some(2048));
    }

    #[test]
    fn quoted_csv_fields_split_on_real_separators_only() {
        assert_eq!(split_csv("a,\"b,c\",d"), ["a", "b,c", "d"]);
        assert_eq!(split_csv("\"a\"\"b\",c"), ["a\"b", "c"]);
        assert_eq!(split_csv(""), Vec::<String>::new());
    }

    #[test]
    fn non_numeric_process_pid_becomes_null() {
        let row = parse_process_row("GPU-x, [N/A], python, 10").unwrap();
        assert_eq!(row.pid, None);
        assert_eq!(row.process_name, "python");
        assert_eq!(row.used_memory_mb, Some(10));
    }

    #[test]
    fn process_without_pid_column_is_still_usable() {
        assert!(parse_process_row("GPU-x").is_none());
        let row = parse_process_row("GPU-x, 7").unwrap();
        assert_eq!(row.pid, Some(7));
        assert_eq!(row.process_name, "");
        assert_eq!(row.used_memory_mb, None);
    }

    #[test]
    fn two_samples_merge_to_the_worst_reading() {
        let text = "\
@@HOST
h
@@NOGPU
0
@@MIG
0
@@GPU
0, GPU-a, X, 1000, 10, 990, 5, 30, Default, 1
@@GPU_RC=0
0, GPU-a, X, 1000, 700, 300, 90, 40, Default, 1
@@GPU_RC=0
@@PROC

@@PROC_RC=0
@@PS
@@END
";
        let report = parse(text);
        let rows = report.gpu_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].memory_used_mb, Some(700));
        assert_eq!(rows[0].memory_free_mb, Some(300));
        assert_eq!(rows[0].util_pct, Some(90));
        assert_eq!(rows[0].temp_c, Some(30));
    }

    #[test]
    fn fallback_marker_switches_sample_layout() {
        let text = "\
@@HOST
h
@@NOGPU
0
@@MIG
0
@@GPU
@@FALLBACK=1
0, GPU-a, X, 1000, 10, 990, 5
@@GPU_RC=0
@@PROC

@@PROC_RC=0
@@PS
@@END
";
        let report = parse(text);
        let rows = report.gpu_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].memory_used_mb, Some(10));
        assert_eq!(rows[0].temp_c, None);
        assert!(report.gpu_samples[0].fallback);
    }

    #[test]
    fn missing_end_is_a_structural_error() {
        let report = parse("@@HOST\nh\n@@NOGPU\n0\n@@GPU\n");
        assert!(report.validate().unwrap_err().contains("@@END"));
    }

    #[test]
    fn missing_nogpu_is_a_structural_error() {
        let report = parse("@@HOST\nh\n@@END\n");
        assert!(report.validate().unwrap_err().contains("@@NOGPU"));
    }

    #[test]
    fn dogpu_one_skips_gpu_requirement() {
        let report = parse("@@HOST\nnas\n@@NOGPU\n1\n@@END\n");
        report.validate().unwrap();
        assert_eq!(report.nogpu, Some(1));
    }

    // The real script exits right after `@@NOGPU 1|2`; this exact shape (no
    // `@@END`, no GPU/PROC/PS sections) is what PRS-03/PRS-04 repro with.
    const REAL_NOGPU_ONE: &str = "@@HOST\nong\n0.1 0.1 0.1 1/1 1\n8\n@@NOGPU\n1\n";
    const REAL_NOGPU_TWO: &str = "@@HOST\nbroken\n0.1 0.1 0.1 1/1 1\n8\n@@NOGPU\n2\n";

    #[test]
    fn real_early_exit_nogpu_one_is_complete() {
        let report = parse(REAL_NOGPU_ONE);
        report.validate().unwrap();
        assert_eq!(report.nogpu, Some(1));
        assert_eq!(report.hostname.as_deref(), Some("ong"));
        assert_eq!(report.cpus, Some(8));
        assert!(!report.ended);
        assert!(!report.saw_gpu_section);
    }

    #[test]
    fn real_early_exit_nogpu_two_is_complete() {
        let report = parse(REAL_NOGPU_TWO);
        report.validate().unwrap();
        assert_eq!(report.nogpu, Some(2));
        assert!(!report.ended);
    }

    #[test]
    fn nogpu_without_a_value_is_still_a_parse_error() {
        let report = parse("@@HOST\nnas\n@@NOGPU\n");
        assert!(report.validate().unwrap_err().contains("@@NOGPU"));
    }

    #[test]
    fn unknown_nogpu_value_is_a_parse_error() {
        let report = parse("@@HOST\nnas\n@@NOGPU\n7\n");
        assert!(report.validate().unwrap_err().contains("@@NOGPU"));
    }

    #[test]
    fn duplicate_gpu_sections_are_a_parse_error() {
        let text = "\
@@HOST
h
@@NOGPU
0
@@MIG
0
@@GPU
0, GPU-a, X, 1000, 10, 990, 5, 30, Default, 1
@@GPU_RC=0
@@GPU
1, GPU-b, X, 1000, 10, 990, 5, 30, Default, 1
@@GPU_RC=0
@@PROC

@@PROC_RC=0
@@PS
@@END
";
        let report = parse(text);
        let err = report.validate().unwrap_err();
        assert!(err.contains("duplicate @@GPU"), "{err}");
    }

    #[test]
    fn driver_failure_carries_noise() {
        let text = "\
@@HOST
h
@@NOGPU
0
@@MIG
0
@@GPU
NVIDIA-SMI has failed because it could not communicate with the driver
@@GPU_RC=9
@@PROC

@@PROC_RC=0
@@PS
@@END
";
        let report = parse(text);
        report.validate().unwrap();
        let (rc, message) = report.gpu_failure().unwrap();
        assert_eq!(rc, 9);
        assert!(message.contains("driver"));
    }

    #[test]
    fn host_facts_survive_a_missing_loadavg() {
        let text = "@@HOST\nnas\n@@NOGPU\n1\n@@END\n";
        let report = parse(text);
        assert_eq!(report.hostname.as_deref(), Some("nas"));
        assert_eq!(report.load1, None);
    }

    #[test]
    fn empty_gpu_section_with_zero_rc_is_empty_not_error() {
        let text =
            "@@HOST\nh\n@@NOGPU\n0\n@@MIG\n0\n@@GPU\n@@GPU_RC=0\n@@PROC\n@@PROC_RC=0\n@@END\n";
        let report = parse(text);
        report.validate().unwrap();
        assert!(report.gpu_rows().is_empty());
        assert!(report.gpu_failure().is_none());
    }
}
