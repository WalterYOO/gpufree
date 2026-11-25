//! ratatui rendering for the list and detail pages (DESIGN §8.1, §8.5).
//!
//! The detail page is built as plain `DetailLine`s first so its branches
//! (ok / error / no_gpu / processes=null) are unit-testable without a
//! terminal.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, Wrap};

use super::{App, DetailView, ListRow, Mode, RowKind};
use crate::model::{Gpu, HostResult, Process, State};
use crate::output::sanitize::{clean, clean_or};
use crate::tui::filter::SortMode;

#[derive(Clone, Debug)]
pub struct DetailLine {
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Debug)]
pub struct DetailDoc {
    pub header: Vec<String>,
    pub body: Vec<DetailLine>,
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks: [Rect; 4] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_header(frame, app, chunks[0]);
    draw_toolbar(frame, app, chunks[1]);
    match app.mode {
        Mode::Detail => draw_detail(frame, app, chunks[2]),
        _ => draw_list(frame, app, chunks[2]),
    }
    draw_footer(frame, app, chunks[3]);

    if app.mode == Mode::Help {
        draw_help(frame, area);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let preset = app.preset.as_deref().unwrap_or("-");
    let left = format!(" gpufree  {preset}  {}", app.transport);
    let right = format!(
        "{}/{} hosts · {} free · {} ",
        app.done_count(),
        app.entries.len(),
        app.total_free(),
        chrono::Local::now().format("%H:%M:%S")
    );
    let pad = (area.width as usize)
        .saturating_sub(left.chars().count() + right.chars().count())
        .max(1);
    let line = Line::from(vec![
        Span::raw(left),
        Span::raw(" ".repeat(pad)),
        Span::raw(right),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().add_modifier(Modifier::BOLD)),
        area,
    );
}

/// The toolbar with the live `matched/total` counter (DESIGN §8.3,
/// TUI-10/TUI-12). `matched` follows text+status filters; `total` is the same
/// data set without them. Neither depends on the scroll offset.
fn toolbar_line(app: &App) -> String {
    let counter = format!("matched {}/{}", app.matched_rows(), app.total_rows());
    if app.mode == Mode::Filter {
        format!(
            "/ {}▏   {counter}   enter apply · esc clear/exit",
            app.query
        )
    } else {
        let query = if app.query.is_empty() {
            "search".to_string()
        } else {
            app.query.clone()
        };
        let sort = match app.sort {
            SortMode::Host => "host",
            SortMode::FreeMem => "free-mem",
        };
        format!(
            "/ {query}   {counter}   f:{}  s:sort {sort}  r:refresh  ?:help",
            app.status.label()
        )
    }
}

fn draw_toolbar(frame: &mut Frame, app: &App, area: Rect) {
    frame.render_widget(Paragraph::new(toolbar_line(app)), area);
}

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::bordered();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let rows = app.visible_rows();
    let body_height = inner.height.saturating_sub(1) as usize;
    app.ensure_visible(body_height);

    let header = Row::new([
        "HOST",
        "IDX",
        "MODEL",
        "FREE/TOTAL",
        "UTIL",
        "TEMP",
        "STATE",
        "USERS",
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let start = app.offset.min(rows.len());
    let end = (start + body_height.max(1)).min(rows.len());
    let table_rows: Vec<Row> = rows[start..end]
        .iter()
        .map(|row| build_row(app, row))
        .collect();

    let widths = [
        Constraint::Length(22),
        Constraint::Length(4),
        Constraint::Length(18),
        Constraint::Length(13),
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Length(15),
        Constraint::Min(6),
    ];
    let selected = app
        .selected
        .checked_sub(start)
        .filter(|index| *index < table_rows.len());
    let mut state = ratatui::widgets::TableState::default().with_selected(selected);
    let table = Table::new(table_rows, widths)
        .header(header)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(table, inner, &mut state);
}

fn build_row<'a>(app: &App, row: &ListRow) -> Row<'a> {
    let entry = &app.entries[row.host];
    let host_cell = Cell::from(clean(&row.label));
    match row.kind {
        RowKind::Pending { .. } => Row::new(vec![
            host_cell,
            Cell::from(app.spinner()),
            Cell::from("probing…"),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
        ]),
        RowKind::Error { .. } => {
            let result = entry.result.as_ref();
            let kind = result
                .and_then(|r| r.error.as_ref())
                .map(|e| e.kind.as_str())
                .unwrap_or("error");
            let secs = result.map(|r| r.duration_ms as f64 / 1000.0).unwrap_or(0.0);
            Row::new(vec![
                host_cell,
                Cell::from("!"),
                Cell::from(format!("{kind} ({secs:.1}s)")),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from("error").style(state_style(app, Color::Red)),
                Cell::from(""),
            ])
        }
        RowKind::NoGpu { .. } => Row::new(vec![
            host_cell,
            Cell::from("—"),
            Cell::from("no GPU"),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from("ok"),
            Cell::from(""),
        ]),
        RowKind::Gpu { gpu, .. } => {
            let result = entry.result.as_ref();
            let Some(info) = result.and_then(|r| r.gpus.get(gpu)) else {
                return Row::new(vec![host_cell]);
            };
            let model = clean_or(info.name.as_deref(), "—");
            let free_total = match (info.memory_free_mb, info.memory_total_mb) {
                (Some(free), Some(total)) => {
                    format!("{:.1}/{:.1}G", free as f64 / 1024.0, total as f64 / 1024.0)
                }
                _ => "—".to_string(),
            };
            let users = match result {
                Some(result) => match &info.processes {
                    None => "?".to_string(),
                    Some(_) => {
                        let users: Vec<String> =
                            result.gpu_users(info).iter().map(|u| clean(u)).collect();
                        if users.is_empty() {
                            "—".to_string()
                        } else {
                            users.join(",")
                        }
                    }
                },
                None => String::new(),
            };
            Row::new(vec![
                host_cell,
                Cell::from(
                    info.index
                        .map(|i| i.to_string())
                        .unwrap_or_else(|| "—".into()),
                ),
                Cell::from(truncate(&model, 17)),
                Cell::from(free_total),
                Cell::from(
                    info.util_pct
                        .map(|u| format!("{u}%"))
                        .unwrap_or_else(|| "—".into()),
                ),
                Cell::from(
                    info.temp_c
                        .map(|t| format!("{t}°"))
                        .unwrap_or_else(|| "—".into()),
                ),
                Cell::from(state_label(info)).style(state_style(app, state_color(info.state))),
                Cell::from(users),
            ])
        }
    }
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let text = match app.mode {
        Mode::Detail => " ↑↓ scroll  PgUp/PgDn  esc back  r refresh",
        Mode::Filter => " type to filter · enter apply · esc clear/exit · ctrl+c quit",
        Mode::Help => " ? / esc / q close help",
        Mode::Normal => " q quit  / search  enter detail  r refresh  f filter  s sort  ? help",
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().add_modifier(Modifier::DIM)),
        area,
    );
}

fn draw_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::bordered();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let Some(doc) = detail_doc(app) else {
        return;
    };
    let chunks: [Rect; 2] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(inner);
    let header: Vec<Line> = doc.header.iter().map(|s| Line::from(s.clone())).collect();
    frame.render_widget(
        Paragraph::new(header).style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );

    let body_height = chunks[1].height as usize;
    let max_scroll = doc.body.len().saturating_sub(body_height);
    if let Some(view) = &mut app.detail {
        view.scroll = view.scroll.min(max_scroll);
    }
    let scroll = app.detail.map(|d| d.scroll).unwrap_or(0) as u16;
    let lines: Vec<Line> = doc
        .body
        .iter()
        .map(|line| Line::from(Span::styled(line.text.clone(), line.style)))
        .collect();
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll, 0))
            .wrap(Wrap { trim: false }),
        chunks[1],
    );
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let width = area.width.saturating_mul(6) / 10;
    let height = area.height.saturating_mul(6) / 10;
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let lines = vec![
        Line::from("gpufree keys").style(Style::default().add_modifier(Modifier::BOLD)),
        Line::from(""),
        Line::from("/          search (subsequence; space = AND)"),
        Line::from("f          cycle all → free → busy → unknown → error"),
        Line::from("s          sort host ↔ free memory"),
        Line::from("enter      open host detail"),
        Line::from("r          re-probe everything"),
        Line::from("j/k ↑/↓    move"),
        Line::from("esc        clear/leave filter · back from detail"),
        Line::from("q          quit (in list) · back (detail) · close help"),
        Line::from("?          this help"),
        Line::from("ctrl+c     quit"),
    ];
    let block = Block::bordered().title(" Help ");
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

fn state_color(state: State) -> Color {
    match state {
        State::Free => Color::Green,
        State::Busy => Color::DarkGray,
        State::Unknown => Color::Yellow,
    }
}

fn state_style(app: &App, color: Color) -> Style {
    if app.color {
        Style::default().fg(color)
    } else {
        Style::default()
    }
}

fn state_label(gpu: &Gpu) -> String {
    match (gpu.state, gpu.reason.as_deref()) {
        (State::Unknown, Some("mig")) => "unknown MIG".to_string(),
        (State::Unknown, Some(reason)) => format!("unknown ({reason})"),
        _ => gpu.state.as_str().to_string(),
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn plain(text: impl Into<String>) -> DetailLine {
    DetailLine {
        text: text.into(),
        style: Style::default(),
    }
}

fn styled(text: impl Into<String>, color: Color) -> DetailLine {
    DetailLine {
        text: text.into(),
        style: Style::default().fg(color),
    }
}

/// Build the detail document for the current selection (DESIGN §8.5).
pub fn detail_doc(app: &App) -> Option<DetailDoc> {
    let view = app.detail?;
    let entry = app.entries.get(view.host)?;
    let preset = app.preset.as_deref().unwrap_or("-");
    let host = clean(&entry.meta.host);

    let mut header = vec![format!("gpufree  {preset} / {host}")];
    match &entry.result {
        Some(result) => {
            header.push(format!(
                "alias {} · transport {} · {:.1}s · probed {}",
                clean(&entry.meta.resolved_alias),
                entry.meta.transport_used,
                result.duration_ms as f64 / 1000.0,
                result.probed_at
            ));
            let mut third = format!(
                "hostname {} · load {} / {} cpus",
                clean_or(result.hostname.as_deref(), "—"),
                result
                    .load1
                    .map(|l| format!("{l:.2}"))
                    .unwrap_or_else(|| "—".into()),
                result
                    .cpus
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "—".into())
            );
            if result.is_ok() {
                if !result.fields_complete {
                    third.push_str("  ·  ! 部分 GPU 字段缺失");
                }
                if !result.processes_complete {
                    third.push_str("  ·  ! 进程列表不完整");
                }
            }
            header.push(third);
        }
        None => {
            header.push(format!(
                "alias {} · transport {} · probing…",
                clean(&entry.meta.resolved_alias),
                entry.meta.transport_used
            ));
            header.push("hostname — · load — / — cpus".to_string());
        }
    }

    let body = match &entry.result {
        None => vec![plain("探测尚未完成…")],
        Some(result) if !result.is_ok() => error_body(result),
        Some(result) if result.gpus.is_empty() => vec![plain("未检测到 GPU")],
        Some(result) => gpu_body(result, view),
    };
    Some(DetailDoc { header, body })
}

fn error_body(result: &HostResult) -> Vec<DetailLine> {
    let error = result.error.as_ref();
    vec![
        styled("ERROR", Color::Red),
        plain(format!(
            " kind: {}",
            error.map(|e| e.kind.as_str()).unwrap_or("unknown")
        )),
        plain(format!(
            " message: {}",
            clean(error.map(|e| e.message.as_str()).unwrap_or(""))
        )),
        plain(format!(
            " retryable: {}",
            match error {
                Some(e) if e.retryable => "yes",
                _ => "no",
            }
        )),
    ]
}

fn gpu_body(result: &HostResult, view: DetailView) -> Vec<DetailLine> {
    let mut body = Vec::new();
    body.push(styled("GPU", Color::Cyan));
    body.push(plain(format!(
        "   IDX  {:<18}  {:>13}  {:>4}  {:>4}  STATE",
        "MODEL", "FREE/TOTAL", "UTIL", "TEMP"
    )));
    let selected = view.gpu.filter(|index| *index < result.gpus.len());
    for (index, gpu) in result.gpus.iter().enumerate() {
        let marker = if selected == Some(index) { ">" } else { " " };
        let line = format!(
            "{marker} {:>3}  {:<18}  {:>13}  {:>4}  {:>4}  {}",
            gpu.index
                .map(|i| i.to_string())
                .unwrap_or_else(|| "—".into()),
            truncate(&clean_or(gpu.name.as_deref(), "—"), 18),
            memory_pair(gpu),
            gpu.util_pct
                .map(|u| format!("{u}%"))
                .unwrap_or_else(|| "—".into()),
            gpu.temp_c
                .map(|t| format!("{t}°"))
                .unwrap_or_else(|| "—".into()),
            detail_state_label(gpu)
        );
        body.push(styled(line, state_color(gpu.state)));
    }

    if let Some(index) = selected {
        let gpu = &result.gpus[index];
        body.push(plain(""));
        body.push(styled(
            format!(
                "SELECTED GPU {}",
                gpu.index
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| index.to_string())
            ),
            Color::Cyan,
        ));
        body.push(plain(format!(
            " uuid {} · compute mode {} · driver {}",
            clean_or(gpu.uuid.as_deref(), "—"),
            clean_or(gpu.compute_mode.as_deref(), "—"),
            clean_or(gpu.driver.as_deref(), "—")
        )));
        body.push(plain(format!(
            " memory {} / {} MiB",
            gpu.memory_used_mb
                .map(|m| m.to_string())
                .unwrap_or_else(|| "—".into()),
            gpu.memory_total_mb
                .map(|m| m.to_string())
                .unwrap_or_else(|| "—".into())
        )));
    }

    body.push(plain(""));
    body.push(styled("COMPUTE PROCESSES", Color::Cyan));
    let all_missing = result.gpus.iter().all(|gpu| gpu.processes.is_none());
    if all_missing {
        body.push(plain(" 进程查询失败（不代表无进程）"));
        return body;
    }
    let mut rows: Vec<(Option<u32>, &Process)> = Vec::new();
    let mut any_missing = false;
    for gpu in &result.gpus {
        match &gpu.processes {
            Some(processes) => {
                for process in processes {
                    rows.push((gpu.index, process));
                }
            }
            None => any_missing = true,
        }
    }
    if any_missing {
        body.push(plain(" 部分 GPU 进程查询失败"));
    }
    if rows.is_empty() {
        body.push(plain(" 无计算进程"));
        return body;
    }
    body.push(plain(format!(
        "  {:<4}  {:<7}  {:<8}  {:<30}  {}",
        "GPU", "PID", "USER", "PROCESS", "MEM"
    )));
    for (gpu, process) in rows {
        body.push(plain(format!(
            "  {:<4}  {:<7}  {:<8}  {:<30}  {}",
            gpu.map(|g| g.to_string()).unwrap_or_else(|| "—".into()),
            process
                .pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "—".into()),
            clean_or(process.user.as_deref(), "—"),
            truncate(&clean(&process.process_name), 30),
            process
                .used_memory_mb
                .map(|m| format!("{:.1}G", m as f64 / 1024.0))
                .unwrap_or_else(|| "—".into())
        )));
    }
    body
}

fn detail_state_label(gpu: &Gpu) -> String {
    match (gpu.state, gpu.reason.as_deref()) {
        (State::Unknown, Some("mig")) => "unknown (MIG)".to_string(),
        (State::Unknown, Some(reason)) => format!("unknown ({reason})"),
        _ => gpu.state.as_str().to_string(),
    }
}

fn memory_pair(gpu: &Gpu) -> String {
    match (gpu.memory_free_mb, gpu.memory_total_mb) {
        (Some(free), Some(total)) => {
            format!("{:.1}/{:.1}G", free as f64 / 1024.0, total as f64 / 1024.0)
        }
        _ => "—".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{TransportMode, TransportUsed};
    use crate::model::{ErrorKind, HostStatus, ProbeErrorInfo};
    use crate::probe::HostMeta;
    use crate::tui::{App, DetailView};

    fn gpu(state: State, procs: Option<Vec<Process>>) -> Gpu {
        Gpu {
            index: Some(3),
            uuid: Some("GPU-3f0a".into()),
            name: Some("H20-3e".into()),
            driver: Some("570.86.15".into()),
            mig: false,
            compute_mode: Some("Default".into()),
            memory_total_mb: Some(143771),
            memory_used_mb: Some(131072),
            memory_free_mb: Some(12699),
            util_pct: Some(0),
            temp_c: Some(33),
            state,
            reason: None,
            processes: procs,
        }
    }

    fn ok_host(gpus: Vec<Gpu>) -> HostResult {
        HostResult {
            host: "gpu06".into(),
            resolved_alias: "gpu06r".into(),
            transport_used: TransportUsed::Remote,
            status: HostStatus::Ok,
            probed_at: "2026-11-25T14:03:06+08:00".into(),
            duration_ms: 7060,
            hostname: Some("gpu06".into()),
            load1: Some(4.0),
            cpus: Some(192),
            fields_complete: true,
            processes_complete: true,
            note: None,
            gpus,
            error: None,
        }
    }

    fn app_with(result: HostResult, gpu: Option<usize>) -> App {
        let meta = HostMeta {
            host: result.host.clone(),
            resolved_alias: result.resolved_alias.clone(),
            transport_used: result.transport_used,
        };
        let mut app = App::new(
            vec![meta],
            Some("company".into()),
            TransportMode::Remote,
            false,
            false,
            None,
        );
        app.entries[0].result = Some(result);
        app.detail = Some(DetailView {
            host: 0,
            gpu,
            scroll: 0,
        });
        app.mode = Mode::Detail;
        app
    }

    fn text(doc: &DetailDoc) -> String {
        let mut out = doc.header.join("\n");
        out.push('\n');
        for line in &doc.body {
            out.push_str(&line.text);
            out.push('\n');
        }
        out
    }

    #[test]
    fn ok_detail_lists_gpu_fields_and_processes() {
        let process = Process {
            gpu_uuid: "GPU-3f0a".into(),
            pid: Some(12345),
            user: Some("root".into()),
            process_name: "python train.py".into(),
            used_memory_mb: Some(131072),
        };
        let app = app_with(
            ok_host(vec![gpu(State::Busy, Some(vec![process]))]),
            Some(0),
        );
        let doc = detail_doc(&app).unwrap();
        let text = text(&doc);
        assert!(text.contains("company / gpu06"));
        assert!(text.contains("alias gpu06r · transport remote"));
        assert!(text.contains("hostname gpu06"));
        assert!(text.contains("SELECTED GPU 3"));
        assert!(text.contains("uuid GPU-3f0a"));
        assert!(text.contains("compute mode Default"));
        assert!(text.contains("driver 570.86.15"));
        assert!(text.contains("memory 131072 / 143771 MiB"));
        assert!(text.contains("python train.py"));
        assert!(text.contains("root"));
        assert!(text.contains("128.0G"));
    }

    #[test]
    fn error_detail_shows_error_block_and_no_gpu_table() {
        let mut result = ok_host(vec![]);
        result.status = HostStatus::Error;
        result.hostname = None;
        result.load1 = None;
        result.cpus = None;
        result.fields_complete = false;
        result.processes_complete = false;
        result.error = Some(ProbeErrorInfo::new(ErrorKind::SshTimeout, "timed out"));
        let app = app_with(result, None);
        let text = text(&detail_doc(&app).unwrap());
        assert!(text.contains("ERROR"));
        assert!(text.contains("ssh_timeout"));
        assert!(text.contains("timed out"));
        assert!(text.contains("retryable: yes"));
        assert!(!text.contains("SELECTED GPU"));
        assert!(!text.contains("COMPUTE PROCESSES"));
    }

    #[test]
    fn no_gpu_detail_says_not_detected() {
        let mut result = ok_host(vec![]);
        result.note = Some("no_gpu".into());
        let app = app_with(result, None);
        let text = text(&detail_doc(&app).unwrap());
        assert!(text.contains("未检测到 GPU"));
        assert!(!text.contains("COMPUTE PROCESSES"));
    }

    #[test]
    fn null_processes_are_reported_as_failure_not_empty() {
        let mut gpu = gpu(State::Unknown, None);
        gpu.reason = Some("no_process_data".into());
        let app = app_with(ok_host(vec![gpu]), Some(0));
        let text = text(&detail_doc(&app).unwrap());
        assert!(text.contains("进程查询失败"));
        assert!(!text.contains("无计算进程"));
        assert!(text.contains("no_process_data"));
    }

    #[test]
    fn empty_process_list_says_no_processes() {
        let app = app_with(ok_host(vec![gpu(State::Free, Some(vec![]))]), Some(0));
        let text = text(&detail_doc(&app).unwrap());
        assert!(text.contains("无计算进程"));
    }

    #[test]
    fn warnings_show_for_incomplete_data() {
        let mut result = ok_host(vec![gpu(State::Free, Some(vec![]))]);
        result.fields_complete = false;
        result.processes_complete = false;
        let app = app_with(result, Some(0));
        let text = text(&detail_doc(&app).unwrap());
        assert!(text.contains("部分 GPU 字段缺失"));
        assert!(text.contains("进程列表不完整"));
    }

    #[test]
    fn toolbar_renders_matched_over_total_and_ignores_scrolling() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut busy = gpu(State::Busy, Some(vec![]));
        busy.name = Some("A100".into());
        let result = ok_host(vec![gpu(State::Free, Some(vec![])), busy]);
        let meta = HostMeta {
            host: result.host.clone(),
            resolved_alias: result.resolved_alias.clone(),
            transport_used: result.transport_used,
        };
        let mut app = App::new(
            vec![meta],
            Some("company".into()),
            TransportMode::Direct,
            false,
            false,
            None,
        );
        app.entries[0].result = Some(result);
        app.query = "a100".into();

        let render = |app: &mut App| {
            let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
            terminal.draw(|frame| draw(frame, app)).unwrap();
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let first = render(&mut app);
        assert!(
            first.contains("matched 1/2"),
            "counter missing from toolbar: {first:?}"
        );

        app.selected = 1;
        app.offset = 1;
        let scrolled = render(&mut app);
        assert!(
            scrolled.contains("matched 1/2"),
            "counter changed with scroll: {scrolled:?}"
        );
    }

    #[test]
    fn detail_sanitizes_control_sequences_from_remote_fields() {
        let process = Process {
            gpu_uuid: "GPU\u{1b}]0;uuid".into(),
            pid: Some(1),
            user: Some("bob\u{7}".into()),
            process_name: "python\u{1b}]2;PWNED\u{7}".into(),
            used_memory_mb: Some(1),
        };
        let mut evil = gpu(State::Busy, Some(vec![process]));
        evil.name = Some("H20\u{1b}]0;PWNED\u{7}".into());
        evil.driver = Some("570\u{7}".into());
        evil.compute_mode = Some("Default\u{1b}".into());
        let mut result = ok_host(vec![evil]);
        result.hostname = Some("gpu\u{1b}]0;x\u{7}".into());
        let app = app_with(result, Some(0));
        let text = text(&detail_doc(&app).unwrap());
        assert!(!text.contains('\u{1b}'), "raw ESC leaked: {text:?}");
        assert!(!text.contains('\u{7}'), "raw BEL leaked: {text:?}");
        assert!(text.contains("PWNED"), "visible text should survive");
    }

    #[test]
    fn error_detail_sanitizes_the_remote_message() {
        let mut result = ok_host(vec![]);
        result.status = HostStatus::Error;
        result.error = Some(ProbeErrorInfo::new(
            ErrorKind::SshTimeout,
            "boom\u{1b}]0;PWNED\u{7}",
        ));
        let app = app_with(result, None);
        let text = text(&detail_doc(&app).unwrap());
        assert!(!text.contains('\u{1b}'), "raw ESC leaked: {text:?}");
        assert!(!text.contains('\u{7}'), "raw BEL leaked: {text:?}");
        assert!(text.contains("boom"));
    }

    #[test]
    fn mig_gpu_is_labelled_in_the_gpu_table() {
        let mut gpu = gpu(State::Unknown, Some(vec![]));
        gpu.reason = Some("mig".into());
        gpu.mig = true;
        let app = app_with(ok_host(vec![gpu]), Some(0));
        let text = text(&detail_doc(&app).unwrap());
        assert!(text.contains("unknown (MIG)"));
    }
}
