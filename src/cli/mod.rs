//! Рендер вывода в строку. Логика — в query.rs. Строки печатает main / отдаёт mcp.

use crate::query::{
    BenchSummary, CacheRow, DbStats, ModelLatency, ParallelTask, SlowRow, SpinRow, TaskBreakdown,
    TaskRow, ToolRow, TopRow, WasteRow,
};
use std::fmt::Write as _;
use std::io::{IsTerminal, Write as _IoWrite};

/// Вывести текст. В терминале — через пейджер (less), иначе — plain.
pub fn page(text: &str) {
    if std::io::stdout().is_terminal() {
        if let Ok(mut child) = std::process::Command::new("less")
            .args(["-R", "-F", "-X"]) // -F: не пейджить если влезает в экран
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return;
        }
    }
    print!("{text}");
}

fn truncate(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

fn fmt_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.0}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn fmt_dur(ms: i64) -> String {
    let s = ms / 1000;
    format!("{}m{:02}s", s / 60, s % 60)
}

pub fn render_stats(s: &DbStats) -> String {
    let mut o = String::new();
    let pct = if s.agent_runs > 0 { 100 * s.agent_runs_linked / s.agent_runs } else { 0 };
    let _ = writeln!(o, "quickai — сводка индекса");
    let _ = writeln!(o, "  проектов:   {}", s.projects);
    let _ = writeln!(o, "  сессий:     {}", s.sessions);
    let _ = writeln!(o, "  задач:      {}", s.tasks);
    let _ = writeln!(o, "  субагентов: {} (тип определён у {} — {}%)", s.agent_runs, s.agent_runs_linked, pct);
    let _ = writeln!(o, "  turn'ов:    {}", s.turns);
    let cr_pct = if s.total_tokens > 0 { 100 * s.total_cache_read / s.total_tokens } else { 0 };
    let _ = writeln!(o, "  ── токены ──");
    let _ = writeln!(o, "  всего:      {}  (input+output+кэш)", fmt_tokens(s.total_tokens));
    let _ = writeln!(o, "    свежие (in+out): {}", fmt_tokens(s.total_in + s.total_out));
    let _ = writeln!(o, "    input:    {}", fmt_tokens(s.total_in));
    let _ = writeln!(o, "    output:   {}", fmt_tokens(s.total_out));
    let _ = writeln!(o, "    cache-read: {} ({}% throughput — перечитывание контекста)", fmt_tokens(s.total_cache_read), cr_pct);
    let _ = writeln!(o, "  ≈ API-эквивалент: ${:.2}  (list-price; подписка — фикс, это объём не счёт)", s.total_cost);
    o
}

pub fn render_top(rows: &[TopRow], group: &str, by: &str) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "Топ по {by} (группа: {group})");
    let _ = writeln!(o, "{:<44} {:>10} {:>10} {:>7}", "ключ", "$", "output", "cnt");
    for r in rows {
        let key: String = r.key.chars().take(42).collect();
        let _ = writeln!(
            o, "{:<44} {:>10.4} {:>10} {:>7}",
            key, r.cost_usd, fmt_tokens(r.out_tokens), r.count
        );
    }
    o
}

pub fn render_tasks(rows: &[TaskRow], title: &str) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "{title}");
    let _ = writeln!(o, "{:<11} {:>8} {:>4} {:>7} {:>8}  {}", "день", "токены", "агн", "wall", "≈$", "задача");
    for r in rows {
        let _ = writeln!(
            o, "{:<11} {:>8} {:>4} {:>7} {:>8}  {}",
            r.day,
            fmt_tokens(r.total_tokens),
            r.agent_count,
            fmt_dur(r.wall_ms),
            format!("${:.2}", r.cost_usd),
            truncate(&r.text, 66)
        );
    }
    o
}

/// Цельная страница-отчёт (как `usage`): сводка + модели + проекты + топ задач + агенты.
pub fn render_usage(
    stats: &DbStats,
    models: &[TopRow],
    projects: &[TopRow],
    tasks: &[TaskRow],
    agents: &[TopRow],
) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "╔══════════════════════════════════════════════════════════╗");
    let _ = writeln!(o, "║  quickai · usage report                                  ║");
    let _ = writeln!(o, "╚══════════════════════════════════════════════════════════╝");
    let _ = writeln!(o);
    o.push_str(&render_stats(stats));
    let _ = writeln!(o, "\n── по моделям ─────────────────────────────────────────────");
    o.push_str(&render_top(models, "model", "cost"));
    let _ = writeln!(o, "\n── по проектам ────────────────────────────────────────────");
    o.push_str(&render_top(projects, "project", "cost"));
    let _ = writeln!(o, "\n── по типам агентов ───────────────────────────────────────");
    o.push_str(&render_top(agents, "agenttype", "cost"));
    let _ = writeln!(o, "\n── топ задач ──────────────────────────────────────────────");
    o.push_str(&render_tasks(tasks, "(по стоимости)"));
    o
}

pub fn render_cache(rows: &[CacheRow]) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "Cache-hit по сессиям (низкий hit% + большой не-кэш = кандидат на фикс)");
    let _ = writeln!(o, "{:>6} {:>10} {:>10}  {:<20} {}", "hit%", "cache-rd", "не-кэш", "проект", "сессия");
    for r in rows {
        let proj: String = r.project.chars().rev().take(18).collect::<String>().chars().rev().collect();
        let _ = writeln!(
            o, "{:>5}% {:>10} {:>10}  {:<20} {}",
            r.hit_pct, fmt_tokens(r.cache_read), fmt_tokens(r.non_cached),
            proj, &r.session_id[..r.session_id.len().min(8)]
        );
    }
    o
}

pub fn render_latency(models: &[ModelLatency], parallel: &[ParallelTask]) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "Латентность — ср. gap между turn'ами по моделям (прокси, idle>120с отрезан)");
    let _ = writeln!(o, "{:<28} {:>8} {:>10}", "модель", "turns", "ср.gap");
    for m in models {
        let _ = writeln!(o, "{:<28} {:>8} {:>9}s", truncate(&m.model, 28), m.turns, m.median_gap_ms / 1000);
    }
    let _ = writeln!(o, "\nПараллелизм субагентов (busy/wall >1 = реально разом)");
    let _ = writeln!(o, "{:>6} {:>4} {:>7} {:>8}  {}", "factor", "агн", "wall", "busy", "задача");
    for p in parallel {
        let _ = writeln!(
            o, "{:>5.1}x {:>4} {:>7} {:>8}  {}",
            p.factor, p.agents, fmt_dur(p.wall_ms), fmt_dur(p.agent_busy_ms), truncate(&p.text, 50)
        );
    }
    o
}

pub fn render_spinning(rows: &[SpinRow]) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "Спиннинг — много turn'ов, низкий output/turn (кружение впустую)");
    let _ = writeln!(o, "{:>6} {:>10} {:>10} {:>4}  {}", "turns", "output", "out/turn", "агн", "задача");
    for r in rows {
        let _ = writeln!(
            o, "{:>6} {:>10} {:>10} {:>4}  {}",
            r.turns, fmt_tokens(r.out_tokens), fmt_tokens(r.out_per_turn), r.agents, truncate(&r.text, 54)
        );
    }
    o
}

pub fn render_tools(rows: &[ToolRow]) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "Профиль тулзов");
    let _ = writeln!(o, "{:<28} {:>10} {:>8} {:>6}", "тулза", "вызовов", "ошибок", "err%");
    for r in rows {
        let _ = writeln!(o, "{:<28} {:>10} {:>8} {:>5}%", truncate(&r.name, 28), r.calls, r.errors, r.err_pct);
    }
    o
}

pub fn render_waste(rows: &[WasteRow]) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "Waste — распределение stop_reason (max_tokens=обрыв/перезапуск, refusal=отказ)");
    let _ = writeln!(o, "{:<20} {:>10}", "stop_reason", "turns");
    for r in rows {
        let _ = writeln!(o, "{:<20} {:>10}", r.reason, r.count);
    }
    o
}

pub fn render_bench(b: &BenchSummary) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "Бенчмарк агента: {}", b.agent_type);
    let _ = writeln!(o, "  запусков:         {}", b.runs);
    let _ = writeln!(o, "  всего:            ${:.2}   {} out   {} turns", b.total_cost, fmt_tokens(b.total_out), b.total_turns);
    let _ = writeln!(o, "  ср. $/запуск:     ${:.4}", b.avg_cost_per_run);
    let _ = writeln!(o, "  ср. turns/запуск: {:.1}", b.avg_turns_per_run);
    let _ = writeln!(o, "  ср. $/turn:       ${:.4}", b.avg_cost_per_turn);
    if !b.days.is_empty() {
        let _ = writeln!(o, "  тренд по дням (посл. 14):");
        let _ = writeln!(o, "    {:<12} {:>6} {:>10}", "день", "runs", "$");
        for d in &b.days {
            let _ = writeln!(o, "    {:<12} {:>6} {:>10.4}", d.day, d.runs, d.cost);
        }
    }
    o
}

pub fn render_task(t: &TaskBreakdown) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "Задача: {}   проект: {}", t.prompt_id, t.project);
    let _ = writeln!(o, "├─ главный агент            ${:.4}", t.main_cost);
    for a in &t.agents {
        // метка: тип агента, иначе промпт-сниппет, иначе hash-id.
        let label = if !a.agent_type.is_empty() {
            a.agent_type.clone()
        } else if !a.prompt.is_empty() {
            truncate(&a.prompt, 40)
        } else {
            a.id.clone()
        };
        let _ = writeln!(
            o, "├─ {:<42} ${:>8.4}  {:>4}t {:>6} {}",
            truncate(&label, 42), a.cost_usd, a.turns, fmt_tokens(a.out_tokens), fmt_dur(a.wall_ms)
        );
    }
    let active = (t.wall_ms - t.idle_ms).max(0);
    let idle_pct = if t.wall_ms > 0 { 100 * t.idle_ms / t.wall_ms } else { 0 };
    let _ = writeln!(o, "──────────────────────────────────────");
    let _ = writeln!(o, "ИТОГО  {} агентов   ${:.4}", t.agents.len(), t.total_cost);
    let _ = writeln!(o, "── время ──");
    let _ = writeln!(o, "  wall (реальное):  {}", fmt_dur(t.wall_ms));
    let _ = writeln!(o, "  активно:          {}", fmt_dur(active));
    let _ = writeln!(o, "  простой (idle):   {} ({}% — харнесс стоял, gap>60с)", fmt_dur(t.idle_ms), idle_pct);
    let _ = writeln!(o, "  макс. единичный простой: {}", fmt_dur(t.max_gap_ms));
    let _ = writeln!(o, "  busy субагентов (сумма): {}  (параллелизм ~{:.1}x)",
        fmt_dur(t.busy_ms), if active > 0 { t.busy_ms as f64 / active as f64 } else { 0.0 });
    o
}

pub fn render_slow(rows: &[SlowRow]) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "Долгие задачи по wall-времени (idle% = сколько харнесс простоял)");
    let _ = writeln!(o, "{:>8} {:>7} {:>5} {:>8} {:>4}  {}", "wall", "idle", "idle%", "макс-gap", "агн", "задача");
    for r in rows {
        let _ = writeln!(
            o, "{:>8} {:>7} {:>4}% {:>8} {:>4}  {}",
            fmt_dur(r.wall_ms), fmt_dur(r.idle_ms), r.idle_pct, fmt_dur(r.max_gap_ms), r.agents, truncate(&r.text, 48)
        );
    }
    o
}
