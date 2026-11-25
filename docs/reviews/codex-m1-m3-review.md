# gpufree M1–M3 Independent Review (Codex)

- **Reviewer:** Codex independent verifier (separate from the implementer)
- **Repository:** `/Users/walter/dsh/temp/gpufree`
- **HEAD:** `1ab8e2d122c77bedbf71d32830c48c5f3fde6afa`
- **Freeze hash (formula from the task):** `c1ed802cc3d15d5c70b5d871a322b01f161e3e40e77b321dd0d94eec195eaf15`
- **Frozen spec:** `docs/DESIGN.md` v1.3, with §16 A1–A25 overriding all earlier examples
- **Plan executed:** `docs/reviews/codex-m1-m3-plan.md` (182 numbered checks)
- **Method:** black-box runs of the release binary against independent fixtures and a purpose-built fake `ssh`; TUI driven through tmux/PTY; source inspection only where the environment cannot reproduce the condition.

> **Remediation update (2026-11-25):** the implementer fixed all 11 first-pass OBJECT IDs and DOC-04b. They were re-verified against freeze `57402634d1671ecd347e6e601d0d0a95a09d61b49927b276af7e5c874240592f` (DESIGN v1.3, A26–A28). **All re-checks PASS; final verdict is now PASS.** See §10. §1–§9 below are the historical first-pass review and are retained for traceability.

---

## 1. Verdict

> **Superseded by §10 (remediation re-check): PASS.**

**OBJECT** — the implementation is broadly functional, but one hard contract is broken and several other requirements fail.

| Scope | PASS | OBJECT |
|---|---:|---:|
| 182 plan checks | **171** | **11** |
| 198 recorded rows (incl. sub-cases) | 184 | 14 |

The 11 failing plan IDs are: `PRS-02`, `PRS-03`, `PRS-04`, `PRS-12`, `PRS-15`, `TUI-10`, `TUI-12`, `DOC-03`, `DOC-07`, `CFG-13`, `SEC-08`. Three additional sub-case rows also fail (`PRS-03-real`, `PRS-04-real`, `DOC-03b`).

**Hard contract failures:**
1. **Real no-`nvidia-smi` hosts are reported as `parse_error` instead of `ok/no_gpu`** (or `nvidia_smi_missing`). This breaks the state/JSON contract and makes the real-world NAS use case wrong (`PRS-03`, `PRS-04`, `DOC-07`).
2. **Sub-second `sample_interval` is silently replaced by 1 s** (`PRS-02`), contradicting A13/A25.
3. **Quoted-CSV process names are mangled** (`PRS-15`), violating the exact-UTF-8 process name contract (A11).
4. **TUI has no `matched/total` counter** (`TUI-10`, `TUI-12`), explicitly required by §8.3:511.
5. **Remote terminal control sequences pass through to the table/TUI unfiltered** (`SEC-08`), a terminal-injection robustness/security defect.

No source changes occurred during the review; the freeze is intact (details in §2). Final verdict is **OBJECT**, not **BLOCKED**.

---

## 2. Freeze and quality gates

### 2.1 Freeze

Recomputed exactly as specified:

```sh
{ git diff; git ls-files -o --exclude-standard -z | xargs -0 shasum -a 256; } | shasum -a 256
```

- At review start: `c1ed802cc3d15d5c70b5d871a322b01f161e3e40e77b321dd0d94eec195eaf15` — **match**.
- At review end (before writing this file): recomputed `c1ed802c…eaf15` — **match**.
- The only file added afterwards is this allowed review file; a recomputation excluding it is identical. The implementer's sources and tests were not touched.

### 2.2 Gates

| Gate | Result |
|---|---|
| `cargo fmt --check` | PASS (rc 0) |
| `cargo clippy --all-targets -- -D warnings` | PASS (rc 0, no warnings) |
| `cargo test` | PASS — **159** tests (120 unit + 36 integration + 3 golden), 0 failed |
| `cargo build --release` | PASS (rc 0) |

### 2.3 Do the tests actually assert the behaviour?

Yes in aggregate: 159 tests contain ~546 assertions; the integration suite drives the real binary with a fake `ssh` and checks JSON values, exit codes, argv, and signal behaviour. This is substantially better than smoke tests.

However, the green suite **encodes the two main defects**, which is why 159/159 can pass while the product is wrong:

- `src/probe/mod.rs:589` and `tests/cli.rs:287-310` feed `@@NOGPU 1` **followed by `@@END`**, a shape the real script never emits (`src/probe/script.rs:24-27` exits immediately). The test therefore validates the parser against the implementer's fixture, not against the script.
- `src/probe/script.rs` tests only exercise whole-second intervals (`sleep 1`, `sleep 2`); there is no 100–999 ms case, so `interval.as_secs().max(1)` is untested.
- `tests/cli.rs:392-405` (`commas_in_names_are_preserved`) uses an **unquoted** comma name. The quoted CSV form that `nvidia-smi` actually emits is not tested, which hides `PRS-15`.
- TUI tests (17 in `src/tui/mod.rs`, 7 in `src/tui/ui.rs`, 4 in `src/tui/filter.rs`) are pure state/render-buffer tests; there is no PTY-driven test (the implementer declared this gap). We covered the TUI through tmux, but no test protects it.
- No test exercises real ControlMaster reuse, D-state reaping, or pgid reuse.

---

## 3. Method and coverage

- 198 recorded rows: all 182 plan IDs plus 16 explicit sub-cases (e.g. `CLI-07a/b/c`, `SSH-22a/b`, `PRS-03-real`).
- All fixtures live in `/tmp/gpufree-review`; none of the implementer's `testdata/` or `fake-ssh` values were used as expected values.
- The fake `ssh` records argv, stdin, PID/PPID/PGID, timing, and can delay, ignore signals, spawn children, flood either stream, return arbitrary stderr, and exit with arbitrary codes.
- SSH lifecycle checks were run with real process-group inspection (`ps`), real SIGINT/SIGTERM, and real timeouts. TUI checks used tmux and a PTY, including 80×24 and 40×10.
- Auto-transport checks used a real localhost listener; no dependency on the real 4090-1 host.

---

## 4. OBJECT details

### OBJ-1 — Real no-GPU hosts become `parse_error` (highest severity)

**IDs:** `PRS-03`, `PRS-03-real`, `PRS-04`, `PRS-04-real`, `DOC-07`
**Impact:** R1 (host status), R2 (JSON `status`/`error.kind`), R3/R5 (no_gpu detail unreachable on real hosts), M3.

**Repro:**

```sh
python3 /tmp/gpufree-review/repro_objects.py   # section "NOGPU-real"
```

The fixture is exactly the real script's early-exit shape (`docs/DESIGN.md:282-286`):

```
@@HOST
ong
0.1 0.1 0.1 1/1 1
8
@@NOGPU
1
```

- **Expected:** `status=ok`, `gpus=[]`, `note=no_gpu`, `fields_complete=processes_complete=true`, exit 0 (A1/A18, PRS-03). For `@@NOGPU 2`: `status=error`, `kind=nvidia_smi_missing`, `retryable=false` (PRS-04).
- **Actual:** both become `status=error`, `kind=parse_error`, message `output truncated: missing @@END`; exit 1.
- **Cause:** `src/probe/script.rs:24-27` prints `@@NOGPU` then `exit 0`, so no `@@GPU/@@PROC/@@PS/@@END` follows. `src/probe/parse.rs:59-62` unconditionally requires `ended` (`@@END`) before considering `nogpu`. `src/probe/mod.rs:282-291` validates before classifying.
- **Why the tests miss it:** `src/probe/mod.rs:589,604` and `tests/cli.rs:287-310` append `@@END` to the NOGPU fixture, a shape only the tests produce.
- **Doctor consequence:** `doctor --probe` prints `ERROR parse_error (retryable=false) output truncated: missing @@END` for a real NAS instead of `ok · no GPU` (`src/doctor.rs:178-194`), so DOC-07's "nvidia-smi present?" check cannot answer for real no-GPU hosts.

### OBJ-2 — Sub-second `sample_interval` becomes 1 s

**ID:** `PRS-02`
**Impact:** R1 (sampling fidelity; A13 allows 100 ms–10 s).

**Repro:** `python3 /tmp/gpufree-review/repro_objects.py` (section `PRS-02`). Config: `samples=3`, `sample_interval="500ms"`. The captured stdin contains:

```sh
[ "$done_samples" -lt 3 ] && sleep 1
```

- **Expected:** `sleep 0.5` (or equivalent 500 ms).
- **Actual:** `sleep 1` for every interval below 1 s (`100ms` → 1 s).
- **Cause:** `src/probe/script.rs:14` `let sleep_secs = interval.as_secs().max(1);` truncates to whole seconds and floors at 1. The config layer correctly accepts 500 ms (`src/config.rs:83-106,406-410`), so this is a silent semantic change, not a usage error.
- **Spec:** A13 `docs/DESIGN.md:687`; A25 `docs/DESIGN.md:699` ("实际脚本必须按配置写入 samples / sample_interval").

### OBJ-3 — Quoted CSV process names are mangled

**ID:** `PRS-15`
**Impact:** R2 (A11 exact UTF-8 `process_name`), R1/R5 display.

**Repro:** `python3 /tmp/gpufree-review/repro_objects.py` (section `PRS-15`). Input row:

```
GPU-0000, 1234, "python train.py --name ""a,b""", 100
```

- **Expected:** `python train.py --name "a,b"`.
- **Actual:** `"python train.py --name ""a, b"""` (outer quotes retained, quoted comma split and rejoined with `, `).
- **Cause:** `src/probe/parse.rs:293-303` splits the line on every `,` and rejoins `parts[2..len-1]` with `", "`; it does not implement CSV quote/escape handling. Unquoted commas are normalized to `", "`, which is tolerable; quoted CSV is not.
- **Test gap:** `tests/cli.rs:392-405` and `src/probe/parse.rs:498-505` only cover unquoted commas.

### OBJ-4 — Duplicate `@@GPU` section is classified `nvidia_smi_failed`, not `parse_error`

**ID:** `PRS-12`
**Impact:** R2 error classification (low real-world reachability; still a parser contract).

**Repro:** `python3 /tmp/gpufree-review/repro_objects.py` (section `PRS-12`). Two `@@GPU` sections, both with `@@GPU_RC=0`.

- **Expected:** `parse_error`, `retryable=false` (PRS-12).
- **Actual:** `status=error`, `kind=nvidia_smi_failed`, message `nvidia-smi return code missing`.
- **Cause:** `src/probe/parse.rs:149-151` does not detect a repeated `@@GPU`; `179-189` starts a new sample on the next row, leaving it with `rc=None`; `73-90` turns missing rc into `nvidia_smi_failed`.
- **Note:** missing `@@END` and `@@NOGPU` without a value do produce `parse_error`; only the duplicate-section case is misclassified.

### OBJ-5 — `ssh -G` failure is not a doctor failure

**ID:** `DOC-03` (sub-case `DOC-03b`)
**Impact:** M3/R1 diagnostics; §9 item 2, A6.

**Repro:**

```sh
# ssh-gfail exits 255 for -G and prints "Could not resolve hostname"
/tmp/gpufree-review/bin/ssh-gfail ...   # used via ssh_binary in the config
gpufree doctor
```

- **Expected:** non-zero exit; message naming the preset and host; not "all checks passed".
- **Actual:** `rc=0`, `doctor: all checks passed`, endpoint printed as `?`.
- **Cause:** `src/doctor.rs:137-154`: `resolver.ssh_g()` returning `None` only changes the displayed endpoint to `?`; it never increments `failures`. The missing-`remote_aliases` half of DOC-03 passes (exit 2 with `presets.p … requires remote_aliases.nomap`).

### OBJ-6 — `auto` TCP decision is not cached for 60 s

**ID:** `CFG-13`
**Impact:** low — repeated 800 ms probes and doctor latency; §4:201 says the result is cached 60 s.

**Repro:** two presets containing the same alias, `transport="auto"`, localhost listener; run `gpufree --print-config`.

- **Expected:** one `ssh -G` and one TCP accept (60 s in-process cache).
- **Actual:** one `ssh -G`, **two TCP accepts**.
- **Cause:** `src/probe/transport.rs:99-108` caches only `SshConfig`; `129-131` constructs a fresh `tcp_probe` and `226-236` calls it for every `resolve_auto`. `src/run.rs:154` resolves each preset independently. The determination is still correct; only the cache is incomplete.
- **Interpretation note:** if "结果" in §4:201 is read as only the `ssh -G` result, this is a MISSING-NUANCE rather than a defect; the plan's CFG-13 explicitly expects one TCP connection, so it is recorded as OBJECT.

### OBJ-7 — TUI lacks the required `matched/total` counter

**IDs:** `TUI-10`, `TUI-12`
**Impact:** R4 (search feedback); §8.3:511.

**Repro:** enter the TUI, type `/ H20`, observe the toolbar.

- **Expected:** a live `matched/total` count after text/status filters (total = current list rows, error rows included).
- **Actual:** toolbar only shows `/ <query>  f:<status>  s:sort <mode>  r:refresh  ?:help`; no counter in list, filter, or detail mode.
- **Cause:** `src/tui/ui.rs:76-94` never renders a counter; `App::matched_hosts` exists at `src/tui/mod.rs:338-345` but is unused. The AND semantics of multi-keyword search itself passes.

### OBJ-8 — Remote terminal control sequences are emitted verbatim

**ID:** `SEC-08`
**Impact:** terminal injection / robustness (R1/R2). §10 does not state this explicitly; the plan treats it as a required robustness property.

**Repro:** `python3 /tmp/gpufree-review/repro_objects.py` (section `SEC-08`). A GPU name contains `ESC ] 0 ; PWNED BEL`.

- **Expected:** control bytes stripped or escaped before table/TUI rendering.
- **Actual:** table stdout contains raw `0x1b` and the literal `PWNED`; a real terminal would execute the OSC sequence (title change, potentially worse). JSON is safe because JSON escapes control characters.
- **Cause:** `src/output/table.rs:78,91-95` interpolates `gpu.name`/`host.host` directly; no sanitizer in `src/probe/parse.rs:316-320`. The TUI renders the same model strings.

---

## 5. Per-ID verdict table

| CLI | 20 | 20 | 0 |
| CFG | 19 | 18 | 1 |
| SSH | 25 | 25 | 0 |
| PRS | 21 | 16 | 5 |
| STA | 14 | 14 | 0 |
| JSN | 20 | 20 | 0 |
| TUI | 21 | 19 | 2 |
| DET | 12 | 12 | 0 |
| DOC | 15 | 13 | 2 |
| SEC | 10 | 9 | 1 |
| TBL | 5 | 5 | 0 |

### CLI — §3 CLI / exit codes (20 checks)

| ID | Verdict | Evidence / note |
|----|---------|-----------------|
| CLI-01 | PASS | help rc=0 has-usage=True; version rc=0 out='gpufree 0.1.0' |
| CLI-02 | PASS | rc=0 hosts=['clifree', 'clibusy'] preset=p |
| CLI-03 | PASS | rc=2, stdout empty, stderr lists usage examples and 'No default_preset is configured in <path>' (config example provide… |
| CLI-04 | PASS | all five sources (--preset/--host/--hosts/positional/--hosts-file) resolve to ['clia','clib'] |
| CLI-05 | PASS | 10 pairs, failures=[] |
| CLI-06 | PASS | [(['--host', ''], 2, False), (['--hosts', ''], 2, False), (['--hosts', ',,'], 2, False), (['--hosts-file', '/tmp/gpufre… |
| CLI-07 | PASS | usage errors: unknown flag exit 2, missing value exit 2; bare word is a host positional (documented) |
| CLI-08 | PASS | rc=2 stderr='gpufree: preset "nope" not found; see `gpufree presets list` (/tmp/gpufree-review/xdg/cli/gpufree/config.t… |
| CLI-09 | PASS | cli invalid rc=2; config invalid rc=2 err='gpufree: cannot parse config /tmp/gpufree-review/xdg/cli09/gpufree/config.to… |
| CLI-10 | PASS | rc=2 stdout='' stderr='gpufree: cannot parse config /tmp/gpufree-review/xdg/cli10/gpufree/config.toml: TOML parse error… |
| CLI-11 | PASS | all-fail rc=1 hosts=2 |
| CLI-12 | PASS | partial rc=0 error=1 |
| CLI-13 | PASS | strict partial rc=1; strict+require-free(no free, one error) rc=1 |
| CLI-14 | PASS | all-fail+require-free rc=1; ok-busy+error+require-free rc=3 |
| CLI-15 | PASS | busy+require-free rc=3 |
| CLI-16 | PASS | busy no-require rc=0 |
| CLI-17 | PASS | free+require-free rc=0 |
| CLI-18 | PASS | table hosts lines=3 has clibusy=False; json shown=1 rc=0 |
| CLI-19 | PASS | usage+json stdout=''; config+json stdout='' |
| CLI-20 | PASS | json+format conflict rc=2 stderr="error: the argument '--json' cannot be used with '--format <FORMAT>'"; --format json … |

### CFG — §4 config (19 checks)

| ID | Verdict | Evidence / note |
|----|---------|-----------------|
| CFG-01 | PASS | XDG rc=0; HOME fallback rc=0 err=''; --config rc=0 |
| CFG-02 | PASS | rc=0 status=ok err='' |
| CFG-03 | PASS | rc=2 err='gpufree: config file not found: /tmp/gpufree-review/nope.toml\n' |
| CFG-04 | PASS | print-config defaults=True; script samples/interval present=True; argv connect/persist=True/True |
| CFG-05 | PASS | rc=2 err='gpufree: invalid configuration in /tmp/gpufree-review/xdg/cfg05/gpufree/config.toml:\ndefault_preset = "ghost… |
| CFG-06 | PASS | 500ms rc=0 argv=['-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=1', '-o']; 1m rc=0; bare "6" rc=0 (accepted as 6s);… |
| CFG-07 | PASS | non-2 results=[]; used_mem_max_mb=-1 rc=2 |
| CFG-08 | PASS | rc=2 err='gpufree: invalid configuration in /tmp/gpufree-review/xdg/cfg08/gpufree/config.toml:\npresets.r: transport = … |
| CFG-09 | PASS | alias=cfgrem argv_alias=True |
| CFG-10 | PASS | rc=0 used=direct alias=cfgauto accepts=1 |
| CFG-11 | PASS | rc=0 used=remote |
| CFG-12 | PASS | rc=2 probed=0 err='gpufree: host "cfgdown": direct connection failed and no remote_aliases entry exists for transport =… |
| CFG-13 | **OBJECT** | -G cached (1 call) but auto TCP decision re-probed per preset: 2 accepts for same alias; §4:201 says result cached 60s |
| CFG-14 | PASS | preset remote + --transport direct -> direct; preset direct + --transport remote -> remote (ssh -G still consulted for … |
| CFG-15 | PASS | preset prefix prepended=True; settings.prefix rc=2 err='gpufree: cannot parse config /tmp/gpufree-review/xdg/cfg15b/gpu… |
| CFG-16 | PASS | hosts=['cfga'] probes=1 (--hosts run probes=1) |
| CFG-17 | PASS | rc=2 err='gpufree: cannot parse config /tmp/gpufree-review/xdg/cfg17/gpufree/config.toml: TOML parse error at line 3, c… |
| CFG-18 | PASS | list rc=0 presets='PRESET               TRANSPORT HOSTS  DESCRIPTION\np                    direct   '; validate good rc… |
| CFG-19 | PASS | validate rc=0 out='configuration valid'; print-config rc=0 err='' |

### SSH — §5.1 ssh lifecycle (25 checks)

| ID | Verdict | Evidence / note |
|----|---------|-----------------|
| SSH-01 | PASS | argv exact match=True |
| SSH-02 | PASS | stdin bytes=1367, last line="echo '@@END'" |
| SSH-03 | PASS | default mux options present |
| SSH-04 | PASS | no-mux argv=['-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=6', '-o', 'ConnectionAttempts=1', '-o', 'ControlMaster=… |
| SSH-05 | PASS | mux=false argv=['-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=6', '-o', 'ConnectionAttempts=1', '-o', 'ControlMast… |
| SSH-06 | PASS | zero argv=['-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=6', '-o', 'ConnectionAttempts=1', '-o', 'ControlMaster=no… |
| SSH-07 | PASS | scanned 72 argv tokens, ControlPersist=0 count=0 |
| SSH-08 | PASS | proxyjump->proxied, proxycommand->proxied, none->direct |
| SSH-09 | PASS | proxycommand none => direct |
| SSH-10 | PASS | peak proxied concurrency=1, elapsed=1028ms (serial if remote=1) |
| SSH-11 | PASS | direct peak=6 (<=6), proxied peak=2 (<=2), overlap=True, rc=0 |
| SSH-12 | PASS | parallel=17 rc=2 (expect 2), 10+6 rc=0 (expect 0) |
| SSH-13 | PASS | same-hop peak=6 (limit 6), rc=0 |
| SSH-14 | PASS | rc=1 kind=ssh_timeout retryable=True elapsed=2051ms err= |
| SSH-15 | PASS | elapsed=2051ms <= 4500 |
| SSH-16 | PASS | rc=130 exit=33ms alive=[] started=8 stderr='' |
| SSH-17 | PASS | rc=130 exit=31ms alive=[] started=8 stderr='' |
| SSH-18 | PASS | TUI q with 4 slow hosts: exit, no fake pids left, alternate screen off, stty restored (TUI-15) |
| SSH-19 | PASS | runs=[(130, 24, 0), (130, 12, 0), (130, 28, 0), (130, 11, 0), (130, 16, 0)] alive_total=0 |
| SSH-20 | PASS | rc=1 kind=ssh_timeout elapsed=1045ms alive=[]; D-state reaper path source-only |
| SSH-21 | PASS | unrelated pid survived=True; disarm-on-normal-completion at src/probe/ssh.rs:226-236 |
| SSH-22 | PASS | 8 MiB cap: 9 MiB stdout (109ms) and 9 MiB stderr (105ms) both output_limit, retryable=false; 4+4 MiB both streams compl… |
| SSH-23 | PASS | malformed host status=error kind=parse_error; other host ok=ok; catch_unwind per host |
| SSH-24 | PASS | 4MiB+4MiB rc=0 kind=None elapsed=102ms |
| SSH-25 | PASS | independent long-lived process survives q; source signals only registered probe pgids, never -O exit. Real ControlMaste… |

### PRS — §5.2/§6 parser + state (21 checks)

| ID | Verdict | Evidence / note |
|----|---------|-----------------|
| PRS-01 | PASS | missing @@END -> parse_error; duplicate @@GPU -> rejected (error), tolerance documented |
| PRS-02 | **OBJECT** | rc=0 script has samples=3=True; sleep lines=['while [ "$done_samples" -lt 3 ]; do', '  [ "$done_samples" -lt 3 ] && sle… |
| PRS-03 | **OBJECT** | synthetic full-protocol @@NOGPU 1 + @@END passes, but the real generated script exits before @@END; real-shape output b… |
| PRS-04 | **OBJECT** | synthetic full-protocol @@NOGPU 2 + @@END gives nvidia_smi_missing, but real-shape output (no @@END) becomes parse_erro… |
| PRS-05 | PASS | fallback gpu=free driver=None fc=False |
| PRS-06 | PASS | fallback states=free,busy |
| PRS-07 | PASS | double-fail kind=nvidia_smi_failed gpus=[] |
| PRS-08 | PASS | mig gpu=unknown/mig |
| PRS-09 | PASS | proc-fail low=unknown/no_process_data high=busy |
| PRS-10 | PASS | ps-fail status=ok pc=False proc={'gpu_uuid': 'GPU-a', 'pid': 555, 'user': None, 'process_name': 'train.py', 'used_memor… |
| PRS-11 | PASS | pids=[None, None, 123, 321] |
| PRS-12 | **OBJECT** | missing @@END -> parse_error; duplicate @@GPU -> nvidia_smi_failed (wrong kind: "return code missing", expected parse_e… |
| PRS-13 | PASS | status=ok gpus=1 state=unknown |
| PRS-14 | PASS | row={'memory_total_mb': 15360, 'memory_used_mb': None, 'util_pct': 5, 'temp_c': None, 'driver': None} |
| PRS-15 | **OBJECT** | quoted CSV name actual='"python train.py --name ""a, b"""' expected="python train.py --name "a,b""; unquoted comma also… |
| PRS-16 | PASS | utf8 len=4005 |
| PRS-17 | PASS | indices=[7, 2, 2, 5] (stable first-appearance order) |
| PRS-18 | PASS | a=None b='weird host name'/None |
| PRS-19 | PASS | merged util=20 used=600 free=300 (min free raw) |
| PRS-20 | PASS | noise ok=ok; gpu-timeout kind=nvidia_smi_failed |
| PRS-21 | PASS | got={'cltimeout': ('ssh_timeout', True), 'clrefused': ('ssh_unreachable', True), 'clnoroute': ('ssh_unreachable', True)… |

### STA — §6 state machine (14 checks)

| ID | Verdict | Evidence / note |
|----|---------|-----------------|
| STA-01 | PASS | STA-01 states={"sta01": ["free", null], "sta02": ["busy", null], "sta03": ["busy", null], "sta04": ["busy", null], "sta… |
| STA-02 | PASS | STA-02 states={"sta01": ["free", null], "sta02": ["busy", null], "sta03": ["busy", null], "sta04": ["busy", null], "sta… |
| STA-03 | PASS | STA-03 states={"sta01": ["free", null], "sta02": ["busy", null], "sta03": ["busy", null], "sta04": ["busy", null], "sta… |
| STA-04 | PASS | STA-04 states={"sta01": ["free", null], "sta02": ["busy", null], "sta03": ["busy", null], "sta04": ["busy", null], "sta… |
| STA-05 | PASS | STA-05 states={"sta01": ["free", null], "sta02": ["busy", null], "sta03": ["busy", null], "sta04": ["busy", null], "sta… |
| STA-06 | PASS | STA-06 states={"sta01": ["free", null], "sta02": ["busy", null], "sta03": ["busy", null], "sta04": ["busy", null], "sta… |
| STA-07 | PASS | STA-07 states={"sta01": ["free", null], "sta02": ["busy", null], "sta03": ["busy", null], "sta04": ["busy", null], "sta… |
| STA-08 | PASS | STA-08 states={"sta01": ["free", null], "sta02": ["busy", null], "sta03": ["busy", null], "sta04": ["busy", null], "sta… |
| STA-09 | PASS | STA-09 states={"sta01": ["free", null], "sta02": ["busy", null], "sta03": ["busy", null], "sta04": ["busy", null], "sta… |
| STA-10 | PASS | STA-10 states={"sta01": ["free", null], "sta02": ["busy", null], "sta03": ["busy", null], "sta04": ["busy", null], "sta… |
| STA-11 | PASS | STA-11 states={"sta01": ["free", null], "sta02": ["busy", null], "sta03": ["busy", null], "sta04": ["busy", null], "sta… |
| STA-12 | PASS | STA-12 states={"sta01": ["free", null], "sta02": ["busy", null], "sta03": ["busy", null], "sta04": ["busy", null], "sta… |
| STA-13 | PASS | STA-13 states={"sta01": ["free", null], "sta02": ["busy", null], "sta03": ["busy", null], "sta04": ["busy", null], "sta… |
| STA-14 | PASS | summary={'hosts_total': 24, 'hosts_ok': 23, 'hosts_error': 1, 'hosts_shown': 24, 'gpus_total': 23, 'gpus_free': 4, 'gpu… |

### JSN — §7 JSON (20 checks)

| ID | Verdict | Evidence / note |
|----|---------|-----------------|
| JSN-01 | PASS | rc=0 schema=gpufree/v1 jq-parse=True |
| JSN-02 | PASS | missing=[] |
| JSN-03 | PASS | summary={'hosts_total': 9, 'hosts_ok': 8, 'hosts_error': 1, 'hosts_shown': 9, 'gpus_total': 7, 'gpus_free': 2, 'gpus_bu… |
| JSN-04 | PASS | order=['jfree', 'jbusy', 'jnogpu', 'jmig', 'jerr', 'jprocfail', 'jpsfail', 'jfallback', 'jmiss'] |
| JSN-05 | PASS | missing=[] error=None |
| JSN-06 | PASS | error host={'status': 'error', 'hostname': None, 'load1': None, 'cpus': None, 'fields_complete': False, 'processes_comp… |
| JSN-07 | PASS | no_gpu={status:ok,note:no_gpu,fc:True,pc:True} |
| JSN-08 | PASS | proc-fail processes=None, complete empty=[] |
| JSN-09 | PASS | gpu keys missing=[] free.reason=None busy.reason=None |
| JSN-10 | PASS | fallback driver=None temp=None mode=None |
| JSN-11 | PASS | states={'unknown', 'free', 'busy'} |
| JSN-12 | PASS | alias=jremr used=remote request=remote |
| JSN-13 | PASS | local generated=2026-11-25T11:54:45+08:00 probed=2026-11-25T11:54:44+08:00; utc=2026-11-25T03:54:45+00:00 |
| JSN-14 | PASS | top=132 max_host=89 |
| JSN-15 | PASS | A4: filtered hosts=['jfree', 'jfallback'], hosts_shown=2, hosts_total=9, hosts_ok=8, hosts_error=1, gpus_total=7 |
| JSN-16 | PASS | out first/last={} stderr='' |
| JSN-17 | PASS | util=0 used=1 retryable-type=bool |
| JSN-18 | PASS | gpus_total=7 sum=7 |
| JSN-19 | PASS | all-error rc=1 busy rc=0 busy+require-free rc=3 |
| JSN-20 | PASS | NaN/Inf absent=True, top-key occurrences=3 |

### TUI — §8 TUI list (21 checks)

| ID | Verdict | Evidence / note |
|----|---------|-----------------|
| TUI-01 | PASS | non-TTY rc=0 ansi=False |
| TUI-02 | PASS | first frame 126ms; spinner/probing early=True; fixed order=True |
| TUI-03 | PASS | TTY --json alt='0' schema=True tui_border=False |
| TUI-04 | PASS | TTY --format table alt='0' |
| TUI-05 | PASS | header=['gpufree  main  direct                                                                                         … |
| TUI-06 | PASS | segment=['│gpu02 1/3              0    H20-3e             140.4/1', '│                       1    H20-3e             42… |
| TUI-07 | PASS | gpu02 label under search+free filter: True (status=['/ gpu02      f:busy  s:sort host  r:refresh  ?:help']) |
| TUI-08 | PASS | 42.7G row screen offset before=854 after=1613 (free-mem puts low-free GPU later); toolbar=['/ search      f:all  s:sort… |
| TUI-09 | PASS | GPU-name match=True; user match=True; process match=True; host match=True |
| TUI-10 | **OBJECT** | AND semantics (gpu+train keeps busy gpu02 row only)=True; matched/total counter=False (§8.3) |
| TUI-11 | PASS | f cycle labels=['all', 'free', 'busy', 'unknown', 'error', 'all'] |
| TUI-12 | **OBJECT** | matched/total counter present=False; toolbar=['/ H20▏   enter apply · esc clear/exit'] |
| TUI-13 | PASS | GPU detail=True; error detail=True |
| TUI-14 | PASS | detail r returns list=True; new generation probes=9 (expect 9); completes=True |
| TUI-15 | PASS | exited=True alt='0' alt_err='' icanon=True echo=True live=[] started=9 |
| TUI-16 | PASS | normal/filter/detail=[(True, '0', True), (True, '0', True), (True, '0', True)] |
| TUI-17 | PASS | esc1 stays filter=True; esc2 exits=True; q input=True |
| TUI-18 | PASS | ? opens=True; q closes help only=True; esc=True |
| TUI-19 | PASS | toolbar=['/ gpu02      f:all  s:sort free-mem  r:refresh  ?:help'] |
| TUI-20 | PASS | 80x24/40x10 survive=[True, True] |
| TUI-21 | PASS | raw mode restored on q; panic hook + Drop guard source-inspected (src/tui/mod.rs); no forced panic |

### DET — §8.5 detail page (12 checks)

| ID | Verdict | Evidence / note |
|----|---------|-----------------|
| DET-01 | PASS | ['│alias gpu02 · transport direct · 0.1s · probed 2026-11-25T12:01:36+08:00                 ', '│hostname gpu02 · load … |
| DET-02 | PASS | fields warn=True; proc warn=True |
| DET-03 | PASS | GPU rows=['│>   0  H20-3e               140.4/140.4G    0%   29°  free      ', '│    1  H20-3e                42.7/140.… |
| DET-04 | PASS | selected GPU1=True; tier2 dash=True |
| DET-05 | PASS | ['│  1     4242     root      python train.py --lr 0.1        96.7G     '] |
| DET-06 | PASS | null-process text |
| DET-07 | PASS | empty-process text |
| DET-08 | PASS | ['│ kind: ssh_auth                                                                                                     … |
| DET-09 | PASS | no_gpu detail |
| DET-10 | PASS | MIG detail |
| DET-11 | PASS | ssh stdin scripts before=36 after=36 |
| DET-12 | PASS | paged=True; process table reached=False |

### DOC — §9 doctor / init (15 checks)

| ID | Verdict | Evidence / note |
|----|---------|-----------------|
| DOC-01 | PASS | rc=0 all_fields=True probe_ran=False |
| DOC-02 | PASS | syntax+type errors: exit 2, file+key named, no panic |
| DOC-03 | **OBJECT** | missing mapping and -G failure both reported |
| DOC-04 | PASS | missing binary reported; -V failure printed but not counted as failure (see note) |
| DOC-05 | PASS | rc=0 endpoints/proxy classification ok=True; c=['  doc5c                -> doc5c                    direct 192.0.2.9:22'] |
| DOC-06 | PASS | rc=0 direct=True remote=True auto_direct=True auto_remote=True shows_tcp/cache=False |
| DOC-07 | **OBJECT** | rc=1 gpu_metrics=True no_gpu_ok=False no_smi_ok=False lines=['config: /tmp/gpufree-review/xdg/doc7/gpufree/config.toml … |
| DOC-08 | PASS | rc=1 ms=160 hint=True out=['config: /tmp/gpufree-review/xdg/doc8/gpufree/config.toml (loaded)', '  doc8                … |
| DOC-09 | PASS | config_unchanged=True mtime_unchanged=True ssh_dir_unchanged=True |
| DOC-10 | PASS | rc=0 leak=False has_endpoint=True |
| DOC-11 | PASS | rc=0 auto_direct_ok=True cache_shown=False resolved=['#   doc11 -> doc11  transport=direct endpoint=127.0.0.1:52308 pro… |
| DOC-12 | PASS | init_rc=0 created=True perms_ok=True validate_rc=0 out='wrote /tmp/gpufree-review/xdg/doc12/gpufree/config.toml' |
| DOC-13 | PASS | rc=0 unchanged=True stderr='gpufree: config already exists at /tmp/gpufree-review/xdg/doc13/gpufree/config.toml; leavin… |
| DOC-14 | PASS | xdg_ok=True --config_ok=True rc=0/0 realhome_touched=False |
| DOC-15 | PASS | list=True validate_ok=True validate_bad_rc=1 toml_rc=2 |

### SEC — §10 security (10 checks)

| ID | Verdict | Evidence / note |
|----|---------|-----------------|
| SEC-01 | PASS | rc=0 stdin_files=1 bad=[] |
| SEC-02 | PASS | pwn=False per_payload=[('$(touch /tmp/gpu', 1, True, 1, False), ('a; touch /tmp/gp', 1, True, 1, False), ('`touch /tmp/… |
| SEC-03 | PASS | pwn=False results=[('--host', 2, False, False), ('--host=-oProxyCommand=touch /t', 2, False, False)] |
| SEC-04 | PASS | calls=1 all_BatchMode=True ms=156 kind=ssh_auth |
| SEC-05 | PASS | bad_flags=False kind=ssh_hostkey msg='Host key verification failed.' |
| SEC-06 | PASS | leaks=[] lens=[('doctor', 316), ('print', 606), ('json', 276)] |
| SEC-07 | PASS | rc=1 ms=197 kind=output_limit retryable=False json_bytes=857 |
| SEC-08 | **OBJECT** | rc=0 raw_ESC=True contains_PWNED=True |
| SEC-09 | PASS | pipeline_rc=0 stderr='' orphan=[] |
| SEC-10 | PASS | huge=(1, True, 1200) long=(2,True) binhosts=(2,True) csv=(0,True) |

### TBL — §3.2 table / colour (5 checks)

| ID | Verdict | Evidence / note |
|----|---------|-----------------|
| TBL-01 | PASS | rc=0 no_ansi=True has_header/state=True |
| TBL-02 | PASS | always_pipe_ansi=True never_pipe_ansi=False auto_pipe_ansi=True tty_always_ansi=True tty_auto_ansi=True |
| TBL-03 | PASS | NO_COLOR_auto_no_ansi=True always_beats_NO_COLOR=True |
| TBL-04 | PASS | free_only=True hosts=['tok', 'twide'] err_row=['terr     !     ssh_unreachable (0.2s)                              erro… |
| TBL-05 | PASS | rc=0 wide_present=True valid_utf8=True longhost_present=True |

---

## 6. Ambiguities and spec notes

1. **DOC-04b (`ssh -V` nonzero):** with an executable `ssh` whose `-V` exits 1, doctor prints the first stderr line under `ssh version:` but still exits 0 ("all checks passed"). DOC-04 only says "`ssh -V` 失败单独提示"; A6 says doctor exits 1 when a check fails. Recorded PASS (the failure *is* surfaced) but flagged: a strict reading of A6 would make this an OBJECT.
2. **DOC-06 / DOC-11 (auto visibility):** the chosen transport and endpoint are visible; the TCP probe result and cache state are not. §4:201's "doctor/--print-config 可见" is satisfied if it means the determination; if it means the probe/cache detail, it is not. Recorded PASS with this note.
3. **CFG-13 wording:** see OBJ-6. The `ssh -G` cache works; only the TCP decision is re-probed.
4. **JSN-15 (`--free-only`)**: originally ambiguous in §3/§7, resolved by A4. Actual behaviour matches A4 exactly: `hosts` filtered, `hosts_shown=hosts.len()`, `hosts_total/ok/error` and `gpus_*` over the full result. PASS.
5. **PRS-12 duplicate section:** the real script never emits duplicate sections, so the classification bug is only reachable with malformed/tampered output; still contrary to the plan's explicit expectation.
6. **SEC-08:** §10 lists read-only/BatchMode/no-secret/8 MiB/process-group rules but not output sanitisation; the plan's SEC-08 expectation is stricter than §10's literal text. Classified OBJECT as a security/robustness defect, not as a literal §10 violation.
7. **`@@NOGPU` without `@@END`:** this is not an ambiguity — §5.2:282-286 explicitly shows `exit 0` — but it is the single most consequential contradiction between the spec and the parser.

---

## 7. Unverified items and limits

- **Real ControlMaster reuse:** argv and source confirm the intended `ControlMaster`/`ControlPath`/`ControlPersist` options and `--no-mux`/`ControlPersist=0` mappings, but no real ssh multiplexing session was established. Unverified end-to-end.
- **D-state reaper:** `src/probe/ssh.rs:197-219` hands a stuck child to a detached reaper and unregisters the pgid first; inducing a real D-state process is not possible here. Source-inspected only.
- **pgid reuse:** `disarm()` (`src/probe/ssh.rs:222-232`) and `unregister()` (`190-195`) are the mitigation; a real pid/pgid reuse race was not reproduced. Source-inspected only.
- **Real 4090-1 smoke probe:** deliberately not used; all checks are self-contained.
- **TUI terminal-escape execution:** the table path was confirmed to pass raw ESC; the TUI shares the same model strings, but we did not execute an OSC payload in a real terminal.
- **DET-12:** `PgDn` was confirmed to change the detail body, and long-field truncation is present in `src/tui/ui.rs`; reaching the process section at the bottom of a long detail page was not separately captured.
- **8 MiB cap:** 9 MiB on stdout and 9 MiB on stderr each produce `output_limit`; 4 MiB + 4 MiB simultaneously completes. The shared `overflow` flag (`src/probe/ssh.rs:300-302`) is a kill signal only; each reader counts its own stream, so per-stream semantics are correct.
- **SIGKILL/power-loss terminal restore:** covered by the `Drop` guard and panic hook by inspection; not force-tested.

---

## 8. Reproduction

Primary harness (all fixtures are recreated independently):

```sh
python3 /tmp/gpufree-review/batch_doc.py     # DOC-01..15
python3 /tmp/gpufree-review/batch_sec2.py    # SEC-01..10
python3 /tmp/gpufree-review/batch_tbl.py     # TBL-01..05
python3 /tmp/gpufree-review/repro_objects.py # all OBJECT repros
```

Representative raw commands:

```sh
# Real no-GPU shape -> parse_error
printf '@@HOST
nas
0.1 0.1 0.1 1/1 1
8
@@NOGPU
1
' > /tmp/gpufree-review/fixtures/ong.out
gpufree --json --host ong

# 500ms interval -> sleep 1
# (config: sample_interval = "500ms"; inspect the stdin captured by fake ssh)

# ssh -G failure -> doctor says all checks passed
# (config: ssh_binary = "/tmp/gpufree-review/bin/ssh-gfail"; gpufree doctor)
```

`results.json` next to the harness contains the per-ID notes used to build the table above.

---

## 9. Final verdict

**OBJECT.**

The M1/M2 core is strong: SSH argv/lifecycle, cancellation and process-group cleanup, JSON key sets/null semantics/summary math/order, state boundaries and MIG/Prohibited handling, config validation, exit-code precedence, table/colour behaviour, doctor/init/redacted `--print-config`, and almost all TUI/detail behaviour passed independent black-box checks. The implementation is not ready to sign off because real no-`nvidia-smi` hosts are misclassified, which is a hard state/JSON contract failure and a real deployment scenario; sub-second sampling, quoted process names, the TUI counter, auto cache, doctor `-G` failure, and terminal-escape sanitisation also need fixes. After those are addressed, the 11 OBJECT IDs (plus the three dependent sub-cases) should be re-run; no other area needs rework.

---

## 10. Remediation re-check (2026-11-25, second pass)

**Final verdict of this re-check: PASS.** Every first-pass OBJECT and the DOC-04b addendum now pass independent black-box re-checks, and the 46-ID regression subset is clean. No new OBJECT or regression was found.

### 10.1 Freeze integrity

| Item | Value | Result |
|---|---|---|
| `git rev-parse HEAD` | `1ab8e2d122c77bedbf71d32830c48c5f3fde6afa` | unchanged |
| Official freeze hash at start | `57402634d1671ecd347e6e601d0d0a95a09d61b49927b276af7e5c874240592f` | **MATCH** |
| Official freeze hash after all tests, before this section was written | `57402634d1671ecd347e6e601d0d0a95a09d61b49927b276af7e5c874240592f` | **MATCH** |
| Source-only digest (same formula, this review file excluded) before / after | `fb54a6c4c69364f6229312ae0fab6503e46da574809b00861173bf1f8b9e7030` / same | **MATCH** |

This review file is untracked and is part of the official digest, so appending this section necessarily changes the official hash. Excluding only `docs/reviews/codex-m1-m3-review.md` yields a stable source digest that is identical before and after the entire re-check, proving no `src/`, `tests/`, `testdata/`, manifest, or spec file changed while the checks ran.

The tree was not reverted, reformatted, or patched by this reviewer. DESIGN.md is v1.3; A26 (strip C0/C1 for table/TUI, raw JSON), A27 (duplicate `@@GPU` → `parse_error`), and A28 (`ssh -V` nonzero → doctor failure) were applied as the judging criteria.

### 10.2 Quality gates

| Gate | Result |
|---|---|
| `cargo fmt --check` | **PASS** (exit 0) |
| `cargo clippy --all-targets -- -D warnings` | **PASS** (exit 0) |
| `cargo test` | **PASS** — 194 passed, 0 failed (146 unit + 45 CLI + 3 golden) |
| `cargo build --release` | **PASS** (exit 0) |

Test count matches the declared 194. Spot-reading the suites confirmed they assert behaviour, not just exit status: parser tests cover NOGPU-without-END (`real_early_exit_nogpu_one_is_complete`, `real_early_exit_nogpu_two_is_complete`), duplicate `@@GPU` (`duplicate_gpu_sections_are_a_parse_error`), quoted CSV (`quoted_process_name_keeps_a_literal_comma`, `quoted_process_name_unescapes_doubled_quotes`), and fractional `sleep` output (`sleep_seconds_formats_posix_fractions`); CLI tests spawn the binary with a fake ssh and assert JSON/exit codes, including `doctor_fails_when_ssh_g_cannot_resolve_a_host`, `doctor_fails_when_ssh_version_exits_nonzero`, `doctor_probe_treats_a_real_no_gpu_host_as_healthy`, and `sigint_leaves_no_orphan_ssh_processes`; the sanitisation unit tests assert C0/C1 replacement.

### 10.3 The 11 OBJECT IDs + DOC-04b

All were reproduced with this reviewer's own fixtures under `/tmp/gpufree-review/`, not with `testdata/fake-ssh`.

| ID | Check exercised | Evidence (actual) | Verdict |
|---|---|---|---|
| `PRS-02` | `sample_interval = "500ms"` / `"100ms"` | captured ssh stdin contains `sleep 0.5` / `sleep 0.1` (A25); `src/probe/script.rs:65-76` | **PASS** |
| `PRS-03` | Real NOGPU=1 shape, no `@@END`/`@@GPU`/`@@PROC`/`@@PS` | `status=ok`, `note=no_gpu`, `gpus=[]`, `fields_complete=true`, `processes_complete=true`, `hostname=r2-nas`, `cpus=16`, `error=null` | **PASS** |
| `PRS-04` | Real NOGPU=2 shape, single host | single-host `rc=1`; `status=error`, `kind=nvidia_smi_missing`, `retryable=false`, `gpus=[]` | **PASS** |
| `PRS-12` | Duplicate `@@GPU` section | `status=error`, `kind=parse_error`, `retryable=false`, message `duplicate @@GPU section` (A27) | **PASS** |
| `PRS-15` | CSV `"python train.py --name ""a,b"""` | `process_name == python train.py --name "a,b"` exactly (A11) | **PASS** |
| `DOC-03a` | `transport="remote"` without `remote_aliases` | exit 2, `presets.p: transport = "remote" requires remote_aliases.r2-nomap` | **PASS** |
| `DOC-03b` | `ssh -G` failure | exit 1, `ERROR: preset p host r2-ghost: ssh -G r2-ghost failed (alias not resolvable)`, `doctor: 1 check(s) failed` (preset + host named) | **PASS** |
| `DOC-04b` | `ssh -V` exits nonzero | exit 1, `ssh version: FAILED (exit 1): …` (A28) | **PASS** |
| `DOC-07` | `doctor --probe` against no-GPU + normal hosts | exit 0, `r2-nas [direct]: ok · no GPU`, `r2-ok [direct]: ok · 1 GPUs · driver 570.86.15 …`, `all checks passed`; no `parse_error` | **PASS** |
| `CFG-13` | Two identical `transport="auto"` resolutions to one endpoint | exactly 1 `ssh -G` call and 1 TCP accept (60 s cache); `src/probe/transport.rs` `TcpCache` | **PASS** |
| `TUI-10` | Text filter AND semantics + counter | query `tbusy train` → `matched 1/4`, only `tbusy` row visible | **PASS** |
| `TUI-12` | `matched/total` counter | normal `4/4`; text filter `1/4`; status `busy` `1/4`; unchanged `4/4` after 6× `Down` (scroll-invariant) | **PASS** |
| `SEC-08` | ESC/OSC/BEL in hostname, GPU name, user, process name | table and TUI contain U+FFFD, no raw `0x1b`/`0x07`; tmux pane title not hijacked; JSON keeps raw values and emits `\u001b`/`\u0007` (A26) | **PASS** |

### 10.4 Regression subset — 46 IDs

| Group | IDs | Result |
|---|---|---|
| Parser/state | `PRS-01, PRS-05, PRS-07…PRS-11, PRS-13, PRS-14, PRS-16…PRS-21` | **15 PASS** |
| SSH lifecycle | `SSH-14, SSH-16, SSH-18, SSH-20, SSH-22a/b, SSH-23` | **6 PASS** |
| JSON | `JSN-01, JSN-05, JSN-06, JSN-08, JSN-09, JSN-15, JSN-17` | **7 PASS** |
| State machine | `STA-06, STA-09` | **2 PASS** |
| TUI | `TUI-06, TUI-14, TUI-15` | **3 PASS** |
| Detail | `DET-06, DET-07, DET-08, DET-09` | **4 PASS** |
| Doctor/init | `DOC-01, DOC-05, DOC-12, DOC-13` | **4 PASS** |
| Table | `TBL-01, TBL-04` | **2 PASS** |
| Security | `SEC-01, SEC-03, SEC-07` | **3 PASS** |

Key regression evidence: `PRS-01` now correctly classifies duplicate `@@GPU` as `parse_error` (A27), so it is PASS rather than the former "tolerated/ambiguous"; `SSH-14` timeout 2048 ms with `ssh_timeout`/`retryable=true`; `SSH-20` ignore-TERM child 1074 ms with zero survivors; `SSH-16` SIGINT exits 130 in 49 ms with 0 leftover fake pids; `TUI-06` shows `h3 1/3` and the next two rows do not repeat the host; `TUI-14` `r` returns to the list and starts a new probe (1→2); `TUI-15`/`SSH-18` quit cleanly with alternate screen off, `icanon`/`echo` restored, and no fake pids; `DET-06` null-process text, `DET-07` empty-process text, `DET-08` `kind/message/retryable`, `DET-09` no-GPU body all render; `DOC-01` doctor is read-only and does not spawn ssh; `DOC-13` `init` never overwrites; `SEC-01/03/07` still show no injection, no leading-dash passthrough, and the 8 MiB cap.

### 10.5 Exact original failure shapes

1. **NOGPU early exit (no `@@END`)**
   ```sh
   # fixture body (fake ssh + config are supplied by the harness)
   printf '@@HOST\nnas\n0.1 0.1 0.1 1/1 1\n8\n@@NOGPU\n1\n' > /tmp/gpufree-review/fixtures/ong.out
   printf '@@HOST\nnosmi\n0.1 0.1 0.1 1/1 1\n8\n@@NOGPU\n2\n' > /tmp/gpufree-review/fixtures/ons.out
   python3 /tmp/gpufree-review/recheck.py   # PRS-03 -> ok/no_gpu; PRS-04 -> nvidia_smi_missing
   ```
2. **Fractional sleep**: `python3 /tmp/gpufree-review/recheck.py` (PRS-02 cases) — stdin has `sleep 0.5` and `sleep 0.1`.
3. **Quoted CSV**: `python3 /tmp/gpufree-review/recheck.py` (PRS-15) — exact `python train.py --name "a,b"`.
4. **Duplicate section**: `python3 /tmp/gpufree-review/recheck.py` (PRS-12) — `parse_error`, message `duplicate @@GPU section`.
5. **Doctor `-G`/`-V`**: `python3 /tmp/gpufree-review/recheck.py` (DOC-03a/b, DOC-04b) — exit 2 / exit 1 / exit 1 respectively.
6. **Auto cache**: `python3 /tmp/gpufree-review/recheck.py` (CFG-13) — 1 `-G`, 1 TCP accept.
7. **TUI counter**: `python3 /tmp/gpufree-review/recheck_tui.py` — `matched 4/4`, `1/4`, `1/4`, scroll-invariant.
8. **Control characters**: `python3 /tmp/gpufree-review/recheck_sec08.py` — table/JSON/TUI all PASS.

### 10.6 Notes on re-check method and corrected harness expectations

- **DOC-01**: the first-pass harness built `ssh_binary` as `<scratch>/bin/../../fake-ssh`, which resolves to `/tmp/fake-ssh`; that symlink no longer existed, so doctor correctly reported a non-executable binary. Re-run with the real fake-ssh path: exit 0, all fields, no ssh spawned. Product behaviour was already correct.
- **JSN-15**: the stored assertion still expected `hosts=[jfree]`, `hosts_ok=7`, `hosts_error=2`; A4 actually yields `[jfree, jfallback]`, `hosts_shown=2`, `hosts_total=9`, `hosts_ok=8`, `hosts_error=1` because `jfallback` has a free card. Re-verified directly against A4 and PASS.
- **PRS-04**: the batch-level re-check asserted `rc==1` while the same batch also contained a successful host; §3.1 requires exit 0 when at least one host succeeds. The host-level result was always correct, and a single NOGPU=2 host now yields `rc=1` as expected.
- **SEC-10** (outside the required subset): new, documented validations (A14 parallel sum ≤16, non-empty `presets.<p>.hosts`, UTF-8 hosts-file) make the old adversarial configs exit 2. Re-run with a valid settings-only config: 1200 hosts parsed, no panic. Not a defect.
- **tmux mechanics**: this tmux build delivers `C-m` reliably for Enter while the literal `Enter` key name is sometimes dropped; detail checks use `C-m`. This is harness mechanics, not product behaviour.

### 10.7 Still unverified

Unchanged from §7 and not regressions: real ControlMaster reuse over a live ssh connection, a real D-state process handed to the detached reaper, a genuine pid/pgid-reuse race, a live 4090-1 JSON probe, and SIGKILL/power-loss terminal restore. These remain source-inspected only; nothing in the fix set touches them.

### 10.8 Verdict

**PASS.** 58 check IDs / 65 assertions executed in this pass: **65 PASS, 0 OBJECT, 0 AMBIGUOUS, 0 NOT-RUN**. All hard contracts re-checked here (exit-code precedence, JSON schema/nullability, state semantics, ssh orphan/zombie cleanup, security injection, terminal restore) hold. No remaining OBJECT exists, so no remediation follow-up is required for M1–M3.
