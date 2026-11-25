//! The read-only remote script (DESIGN §5.2).
//!
//! One ssh round trip returns host facts, GPU samples, compute processes, and
//! process owners. Sample count and interval are templated in by the client.
//! The script writes section markers (`@@NAME`) and per-query return codes
//! (`@@GPU_RC=`, `@@PROC_RC=`) so error text never masquerades as a data row.

use std::time::Duration;

/// Render the remote script. `prefix` (preset-level, e.g. `module load
/// cuda/12.4 &&`) is prepended by the caller.
pub fn render(samples: u32, interval: Duration) -> String {
    let samples = samples.max(1);
    let pause = sleep_seconds(interval);
    let core_query = "index,uuid,name,memory.total,memory.used,memory.free,utilization.gpu,temperature.gpu,compute_mode,driver_version";
    let fallback_query = "index,uuid,name,memory.total,memory.used,memory.free,utilization.gpu";

    format!(
        r#"echo '@@HOST'
hostname 2>/dev/null
cat /proc/loadavg 2>/dev/null
nproc 2>/dev/null
echo '@@NOGPU'
if ! command -v nvidia-smi >/dev/null 2>&1; then
  if [ -e /dev/nvidia0 ]; then echo 2; else echo 1; fi
  exit 0
fi
echo 0
echo '@@MIG'
nvidia-smi -L 2>/dev/null | grep -q 'MIG' && echo 1 || echo 0
echo '@@GPU'
q='{core_query}'
fb=0; done_samples=0; attempts=0
while [ "$done_samples" -lt {samples} ]; do
  attempts=$((attempts+1))
  if [ "$attempts" -gt {attempt_cap} ]; then break; fi
  out=$(LC_ALL=C nvidia-smi --query-gpu="$q" --format=csv,noheader,nounits 2>&1); rc=$?
  if [ "$rc" -ne 0 ] && [ "$fb" -eq 0 ]; then
    echo '@@FALLBACK=1'
    fb=1
    q='{fallback_query}'
    continue
  fi
  printf '%s\n' "$out"; echo "@@GPU_RC=$rc"
  [ "$rc" -ne 0 ] && break
  done_samples=$((done_samples+1))
  [ "$done_samples" -lt {samples} ] && sleep {pause}
done
echo '@@PROC'
out=$(LC_ALL=C nvidia-smi --query-compute-apps=gpu_uuid,pid,process_name,used_memory --format=csv,noheader,nounits 2>&1); rc=$?
printf '%s\n' "$out"; echo "@@PROC_RC=$rc"
echo '@@PS'
pids=$(nvidia-smi --query-compute-apps=pid --format=csv,noheader,nounits 2>/dev/null | grep -E '^[0-9]+$' | paste -sd, -)
[ -n "$pids" ] && ps -o pid=,user= -p "$pids" 2>/dev/null
echo '@@END'
"#,
        attempt_cap = samples.saturating_add(2),
    )
}

/// POSIX `sleep` argument for `interval`, preserving sub-second values.
///
/// A13 allows 100 ms–10 s; truncating to whole seconds silently turned
/// `500ms` into a one-second pause (review PRS-02).
pub fn sleep_seconds(interval: Duration) -> String {
    let millis = interval.as_millis().max(1);
    if millis % 1000 == 0 {
        (millis / 1000).to_string()
    } else {
        let mut text = format!("{:.3}", millis as f64 / 1000.0);
        while text.ends_with('0') {
            text.pop();
        }
        text
    }
}

/// Script with an optional preset prefix prepended.
pub fn render_prefixed(prefix: Option<&str>, samples: u32, interval: Duration) -> String {
    let body = render(samples, interval);
    match prefix {
        Some(prefix) if !prefix.trim().is_empty() => format!("{}\n{}", prefix.trim_end(), body),
        _ => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_contains_every_section_marker() {
        let s = render(2, Duration::from_secs(1));
        for marker in [
            "@@HOST", "@@NOGPU", "@@MIG", "@@GPU", "@@PROC", "@@PS", "@@END",
        ] {
            assert!(s.contains(marker), "missing {marker}");
        }
        assert!(s.contains("done_samples\" -lt 2"));
        assert!(s.contains("sleep 1"));
        assert!(s.contains("@@FALLBACK=1"));
    }

    #[test]
    fn script_templates_interval_and_samples() {
        let s = render(3, Duration::from_secs(2));
        assert!(s.contains("done_samples\" -lt 3"));
        assert!(s.contains("sleep 2"));
    }

    #[test]
    fn prefix_is_prepended_once() {
        let s = render_prefixed(Some("module load cuda/12.4 &&"), 1, Duration::from_secs(1));
        assert!(s.starts_with("module load cuda/12.4 &&\necho '@@HOST'"));
        let plain = render_prefixed(None, 1, Duration::from_secs(1));
        assert!(plain.starts_with("echo '@@HOST'"));
        let empty = render_prefixed(Some("   "), 1, Duration::from_secs(1));
        assert!(empty.starts_with("echo '@@HOST'"));
    }

    #[test]
    fn sub_second_intervals_reach_the_script_verbatim() {
        // PRS-02: 100ms/500ms must not be floor-rounded to one second.
        let cases = [
            (Duration::from_millis(100), "sleep 0.1"),
            (Duration::from_millis(500), "sleep 0.5"),
            (Duration::from_millis(1500), "sleep 1.5"),
            (Duration::from_secs(1), "sleep 1"),
            (Duration::from_secs(2), "sleep 2"),
        ];
        for (interval, expected) in cases {
            let s = render(2, interval);
            assert!(
                s.contains(expected),
                "{interval:?} should render {expected}"
            );
        }
    }

    #[test]
    fn sleep_seconds_formats_posix_fractions() {
        assert_eq!(sleep_seconds(Duration::from_millis(100)), "0.1");
        assert_eq!(sleep_seconds(Duration::from_millis(500)), "0.5");
        assert_eq!(sleep_seconds(Duration::from_millis(250)), "0.25");
        assert_eq!(sleep_seconds(Duration::from_secs(1)), "1");
        assert_eq!(sleep_seconds(Duration::from_secs(2)), "2");
        assert_eq!(sleep_seconds(Duration::from_millis(10_000)), "10");
        // Zero is clamped to the smallest representable pause, never `sleep 0`.
        assert_eq!(sleep_seconds(Duration::ZERO), "0.001");
    }

    #[test]
    fn fallback_retry_does_not_consume_a_sample() {
        // The retry path must still run when samples == 1.
        let s = render(1, Duration::from_secs(1));
        assert!(s.contains("continue"), "fallback must retry");
    }
}
