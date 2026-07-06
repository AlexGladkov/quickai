//! Генерация HTML-отчёта: сводка + разбивки + все задачи с раскрытием субагентов.
//! Деньги — API-эквивалент (list-price), НЕ реальное списание (Max-подписка фикс).

use crate::query::{
    AgentLine, CacheRow, DbStats, ModelLatency, ParallelTask, SlowRow, SpinRow, TaskRow, ToolRow,
    TopRow, WasteRow,
};
use std::collections::HashMap;
use std::fmt::Write as _;

/// Профилирующие срезы для отчёта (#1–#5 + время).
pub struct Extra {
    pub cache: Vec<CacheRow>,
    pub models: Vec<ModelLatency>,
    pub parallel: Vec<ParallelTask>,
    pub spin: Vec<SpinRow>,
    pub tools: Vec<ToolRow>,
    pub waste: Vec<WasteRow>,
    pub slow: Vec<SlowRow>,
}

fn dur_h(ms: i64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}ч{:02}м", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}м{:02}с", s / 60, s % 60)
    }
}

/// CSS-класс по «плохому» проценту (выше = хуже): error-rate, idle%.
fn bad_hi(pct: i64, warn: i64, bad: i64) -> &'static str {
    if pct >= bad { "bad" } else if pct >= warn { "warn" } else { "" }
}

/// CSS-класс по «хорошему» проценту (выше = лучше): cache-hit.
fn good_hi(pct: i64, warn: i64, bad: i64) -> &'static str {
    if pct <= bad { "bad" } else if pct <= warn { "warn" } else { "good" }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn toks(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.0}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn dur(ms: i64) -> String {
    let s = ms / 1000;
    format!("{}ч{:02}м", s / 3600, (s % 3600) / 60)
}

fn top_table(rows: &[TopRow]) -> String {
    let mut o = String::from("<table><thead><tr><th>ключ</th><th class=n>$ API-экв</th><th class=n>output</th><th class=n>cnt</th></tr></thead><tbody>");
    for r in rows {
        let _ = write!(
            o,
            "<tr><td>{}</td><td class='n money'>${:.2}</td><td class=n>{}</td><td class=n>{}</td></tr>",
            esc(&r.key), r.cost_usd, toks(r.out_tokens), r.count
        );
    }
    o.push_str("</tbody></table>");
    o
}

/// Собрать HTML. tasks — все реальные задачи; agents — map prompt_id → субагенты.
pub fn build(
    stats: &DbStats,
    models: &[TopRow],
    projects: &[TopRow],
    agenttypes: &[TopRow],
    tasks: &[TaskRow],
    agents: &HashMap<String, Vec<AgentLine>>,
    ex: &Extra,
    project: &str,
) -> String {
    let mut o = String::new();
    o.push_str(HEAD);

    // Шапка: имя проекта — крупно и ярко; "quickai" — вторичный eyebrow.
    let title = if project.is_empty() { "Все проекты".to_string() } else { esc(project) };
    let _ = write!(o, r#"<div class=eyebrow>quickai · отчёт по использованию</div><h1>{title}</h1>"#);
    let _ = write!(o, r#"
<div class=note>Деньги ниже — <b>API-эквивалент</b> (сколько эти токены стоили бы по list-price через API).
На Max-подписке <b>$200/мес фиксом</b> реальных списаний нет — это оценка объёма работы, не счёт.</div>"#);

    // Карточки сводки.
    let pct = if stats.agent_runs > 0 { 100 * stats.agent_runs_linked / stats.agent_runs } else { 0 };
    let _ = write!(o, r#"<div class=cards>
<div class=card><div class=k>токенов всего</div><div class=v>{} <span class=sub>in+out+кэш</span></div></div>
<div class=card><div class=k>output</div><div class=v>{}</div></div>
<div class=card><div class=k>input</div><div class=v>{}</div></div>
<div class=card><div class=k>задач</div><div class=v>{}</div></div>
<div class=card><div class=k>субагентов</div><div class=v>{} <span class=sub>тип {}%</span></div></div>
<div class=card><div class=k>сессий</div><div class=v>{}</div></div>
<div class=card><div class=k>проектов</div><div class=v>{}</div></div>
<div class=card><div class=k>≈ API-эквивалент</div><div class=v>${:.0} <span class=sub>не счёт</span></div></div>
</div>"#,
        toks(stats.total_tokens), toks(stats.total_out), toks(stats.total_in),
        stats.tasks, stats.agent_runs, pct, stats.sessions, stats.projects,
        stats.total_cost);

    // Разбивки.
    let _ = write!(o, "<h2>По моделям</h2>{}", top_table(models));
    let _ = write!(o, "<h2>По проектам</h2>{}", top_table(projects));
    let _ = write!(o, "<h2>По типам агентов</h2>");
    let _ = write!(o, r#"<div class=note style="margin:0 0 10px"><b>workflow/без типа</b> — не агент, а бакет субагентов без записанного типа: спавн через <code>Workflow</code>-оркестрацию (фанаут сотен агентов) либо вложенные. Обычные <code>Agent</code>-вызовы линкуются по типу.</div>"#);
    o.push_str(&top_table(agenttypes));

    // ── Профилирующие срезы #1–#5 ──
    let _ = write!(o, "<h2>Профиль тулзов</h2><table><thead><tr><th>тулза</th><th class=n>вызовов</th><th class=n>ошибок</th><th class=n>err%</th></tr></thead><tbody>");
    for t in &ex.tools {
        let _ = write!(o, "<tr><td>{}</td><td class=n>{}</td><td class=n>{}</td><td class='n {}'>{}%</td></tr>",
            esc(&t.name), t.calls, t.errors, bad_hi(t.err_pct, 5, 15), t.err_pct);
    }
    o.push_str("</tbody></table>");

    let _ = write!(o, "<h2>Латентность (ср. gap между turn'ами)</h2><table><thead><tr><th>модель</th><th class=n>turns</th><th class=n>ср.gap</th></tr></thead><tbody>");
    for m in &ex.models {
        let _ = write!(o, "<tr><td>{}</td><td class=n>{}</td><td class=n>{}s</td></tr>",
            esc(&m.model), m.turns, m.median_gap_ms / 1000);
    }
    o.push_str("</tbody></table>");

    let _ = write!(o, "<h2>Параллелизм субагентов</h2><table><thead><tr><th class=n>factor</th><th class=n>агентов</th><th class=n>wall</th><th class=n>busy</th><th>задача</th></tr></thead><tbody>");
    for p in &ex.parallel {
        let _ = write!(o, "<tr><td class=n>{:.1}x</td><td class=n>{}</td><td class=n>{}</td><td class=n>{}</td><td class=t>{}</td></tr>",
            p.factor, p.agents, dur(p.wall_ms), dur(p.agent_busy_ms), esc(&p.text));
    }
    o.push_str("</tbody></table>");

    let _ = write!(o, "<h2>Cache-hit по сессиям (низкий hit% = кандидат на фикс)</h2><table><thead><tr><th class=n>hit%</th><th class=n>cache-read</th><th class=n>не-кэш</th><th>проект</th></tr></thead><tbody>");
    for c in &ex.cache {
        let _ = write!(o, "<tr><td class='n {}'>{}%</td><td class=n>{}</td><td class=n>{}</td><td>{}</td></tr>",
            good_hi(c.hit_pct, 60, 40), c.hit_pct, toks(c.cache_read), toks(c.non_cached),
            esc(c.project.rsplit('-').next().unwrap_or(&c.project)));
    }
    o.push_str("</tbody></table>");

    let _ = write!(o, "<h2>Спиннинг (много turn'ов, низкий output/turn)</h2><table><thead><tr><th class=n>turns</th><th class=n>output</th><th class=n>out/turn</th><th class=n>агентов</th><th>задача</th></tr></thead><tbody>");
    for s in &ex.spin {
        let _ = write!(o, "<tr><td class=n>{}</td><td class=n>{}</td><td class=n>{}</td><td class=n>{}</td><td class=t>{}</td></tr>",
            s.turns, toks(s.out_tokens), toks(s.out_per_turn), s.agents, esc(&s.text));
    }
    o.push_str("</tbody></table>");

    let _ = write!(o, "<h2>Waste (stop_reason)</h2><table><thead><tr><th>reason</th><th class=n>turns</th></tr></thead><tbody>");
    for w in &ex.waste {
        let _ = write!(o, "<tr><td>{}</td><td class=n>{}</td></tr>", esc(&w.reason), w.count);
    }
    o.push_str("</tbody></table>");

    let _ = write!(o, "<h2>Долгие задачи (почему харнесс тормозил)</h2>");
    let _ = write!(o, r#"<div class=note style="margin:0 0 10px"><b>idle%</b> — доля времени, когда харнесс простаивал (нет активности >60с: rate-limit, ожидание, зависание). Высокий idle% = долго НЕ из-за работы.</div>"#);
    let _ = write!(o, "<table><thead><tr><th class=n>wall</th><th class=n>idle</th><th class=n>idle%</th><th class=n>макс-gap</th><th class=n>агентов</th><th>задача</th></tr></thead><tbody>");
    for s in &ex.slow {
        let _ = write!(o, "<tr><td class=n>{}</td><td class=n>{}</td><td class='n {}'>{}%</td><td class=n>{}</td><td class=n>{}</td><td class=t>{}</td></tr>",
            dur_h(s.wall_ms), dur_h(s.idle_ms), bad_hi(s.idle_pct, 50, 90), s.idle_pct, dur_h(s.max_gap_ms), s.agents, esc(&s.text));
    }
    o.push_str("</tbody></table>");

    // Все задачи с раскрытием субагентов.
    let _ = write!(o, r#"<h2>Задачи ({})</h2>
<input id=q placeholder="фильтр по тексту задачи…" oninput=flt()>
<table id=tt><thead><tr>
<th onclick="srt(0)">день</th><th onclick="srt(1)">проект</th>
<th class=n onclick="srt(2,1)">токены</th><th class=n onclick="srt(3,1)">агентов</th>
<th class=n onclick="srt(4,1)">output</th><th class=n onclick="srt(5,1)">wall</th>
<th class=n onclick="srt(6,1)">≈$</th><th>задача</th></tr></thead><tbody>"#, tasks.len());

    for t in tasks {
        let proj = t.project.rsplit('-').next().unwrap_or(&t.project);
        let mut text_cell = esc(&t.text);
        if let Some(ag) = agents.get(&t.prompt_id) {
            if !ag.is_empty() {
                let mut d = String::from("<details><summary>");
                d.push_str(&esc(&t.text));
                d.push_str("</summary><table class=sub><thead><tr><th>агент / промпт</th><th class=n>$</th><th class=n>turns</th><th class=n>out</th><th class=n>wall</th></tr></thead><tbody>");
                for a in ag {
                    // метка: тип агента, иначе промпт-сниппет (workflow), иначе id.
                    let label = if !a.agent_type.is_empty() {
                        a.agent_type.clone()
                    } else if !a.prompt.is_empty() {
                        a.prompt.clone()
                    } else {
                        a.id.clone()
                    };
                    let _ = write!(d, "<tr><td class=t>{}</td><td class=n>${:.2}</td><td class=n>{}</td><td class=n>{}</td><td class=n>{}</td></tr>",
                        esc(&label), a.cost_usd, a.turns, toks(a.out_tokens), dur_h(a.wall_ms));
                }
                d.push_str("</tbody></table></details>");
                text_cell = d;
            }
        }
        let _ = write!(o,
            "<tr><td>{}</td><td>{}</td><td class=n data-v={}>{}</td><td class=n data-v={}>{}</td><td class=n data-v={}>{}</td><td class=n data-v={}>{}</td><td class='n money' data-v={:.4}>${:.2}</td><td class=t>{}</td></tr>",
            t.day, esc(proj),
            t.total_tokens, toks(t.total_tokens),
            t.agent_count, t.agent_count,
            t.out_tokens, toks(t.out_tokens),
            t.wall_ms, dur(t.wall_ms),
            t.cost_usd, t.cost_usd,
            text_cell);
    }
    o.push_str("</tbody></table>");
    o.push_str(FOOT);
    o
}

const HEAD: &str = r#"<!doctype html><html lang=ru><head><meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1">
<title>quickai · отчёт</title>
<link rel=preconnect href=https://fonts.googleapis.com>
<link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&display=swap" rel=stylesheet>
<style>
:root{
  --bg:#191919; --surface:#242322; --card:#212020; --fg:rgba(255,255,255,.92);
  --gray5:#9b9691; --gray3:#6f6b66; --line:rgba(255,255,255,.09);
  --blue:#5b9df5; --blue-bg:rgba(35,131,226,.16); --blue-tx:#6cb0f5;
  --teal:#4ecdc4; --orange:#ff9d54; --green:#5bd67d; --red:#f87171;
  --shadow:rgba(0,0,0,.28) 0 4px 18px, rgba(0,0,0,.18) 0 1px 4px;
}
*{box-sizing:border-box}
body{margin:0 auto;max-width:1180px;padding:44px 40px 80px;background:var(--bg);color:var(--fg);
  font-family:Inter,-apple-system,system-ui,Segoe UI,Helvetica,Arial,sans-serif;
  font-size:15px;line-height:1.5;font-feature-settings:"lnum","locl";
  -webkit-font-smoothing:antialiased}
.eyebrow{font-size:13px;font-weight:600;letter-spacing:.3px;color:var(--gray5);
  text-transform:uppercase;margin-bottom:6px}
h1{font-size:56px;font-weight:700;letter-spacing:-2px;line-height:1;margin:0 0 24px;color:#fff}
h2{font-size:22px;font-weight:700;letter-spacing:-.4px;margin:52px 0 14px;color:#fff}
.note{background:var(--surface);border:1px solid var(--line);border-radius:12px;
  padding:14px 18px;color:var(--gray5);margin-bottom:22px;font-size:14px}
.note b{color:var(--fg);font-weight:600}
.note code{background:#2f2e2c;border:1px solid var(--line);border-radius:4px;padding:1px 6px;
  font-size:12px;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;color:#e6d9c8}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(168px,1fr));gap:14px;margin:24px 0 8px}
.card{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:18px 20px;box-shadow:var(--shadow)}
.card .k{color:var(--gray5);font-size:13px;font-weight:500}
.card .v{font-size:28px;font-weight:700;letter-spacing:-.6px;margin-top:6px;color:#fff}
.card .sub{font-size:12px;color:var(--gray3);font-weight:400;letter-spacing:.1px}
table{width:100%;border-collapse:collapse;margin-bottom:8px;
  border:1px solid var(--line);border-radius:12px;overflow:hidden;box-shadow:var(--shadow);background:var(--card)}
thead tr{background:var(--surface)}
th,td{text-align:left;padding:9px 14px;border-bottom:1px solid var(--line);vertical-align:top}
tbody tr:last-child td{border-bottom:none}
tbody tr:hover{background:var(--surface)}
th{color:var(--gray5);font-weight:600;cursor:pointer;user-select:none;font-size:12px;letter-spacing:.1px;
  white-space:nowrap}
td{font-size:14px}
td.n,th.n{text-align:right;font-variant-numeric:tabular-nums lining-nums}
td.t{max-width:560px;color:var(--gray5)}
.money{font-weight:600;color:#fff}
.pill{display:inline-block;padding:2px 9px;border-radius:9999px;font-size:12px;font-weight:600;letter-spacing:.1px}
.good{color:var(--teal)} .warn{color:var(--orange);font-weight:600} .bad{color:var(--red);font-weight:700}
#q{width:100%;padding:11px 14px;margin-bottom:12px;background:var(--card);border:1px solid var(--line);
  border-radius:8px;color:var(--fg);font-size:14px;font-family:inherit;outline:none;transition:.15s}
#q:focus{border-color:var(--blue);box-shadow:0 0 0 3px var(--blue-bg)}
#q::placeholder{color:var(--gray3)}
details summary{cursor:pointer;color:var(--fg);list-style:none}
details summary::-webkit-details-marker{display:none}
details summary::before{content:"▸ ";color:var(--gray3)}
details[open] summary{color:var(--blue);margin-bottom:8px}
details[open] summary::before{content:"▾ ";color:var(--blue)}
table.sub{margin:8px 0 4px;background:var(--surface);border:1px solid var(--line);
  border-radius:10px;box-shadow:none}
table.sub thead tr{background:transparent}
table.sub th,table.sub td{font-size:12.5px;padding:6px 10px}
</style></head><body>"#;

const FOOT: &str = r#"<script>
function flt(){let q=document.getElementById('q').value.toLowerCase();
for(let r of document.querySelectorAll('#tt tbody tr')){
 r.style.display=r.lastElementChild.textContent.toLowerCase().includes(q)?'':'none';}}
function srt(i,num){let tb=document.querySelector('#tt tbody');
let rows=[...tb.querySelectorAll('tr')];
rows.sort((a,b)=>{let x=a.children[i],y=b.children[i];
 if(num){return (+y.dataset.v||0)-(+x.dataset.v||0);}
 return x.textContent.localeCompare(y.textContent);});
rows.forEach(r=>tb.appendChild(r));}
</script></body></html>"#;
