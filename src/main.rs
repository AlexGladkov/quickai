//! quickai — профайлер работы Claude Code.

mod cli;
mod export;
mod index;
mod mcp;
mod model;
mod parse;
mod pricing;
mod query;
mod report;
mod source;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "quickai", version, about = "Профайлер работы Claude Code: токены, деньги, агенты")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Построить/обновить индекс из транскриптов источника
    Index {
        /// Снести и перечитать всё с нуля (в пределах источника)
        #[arg(long)]
        rebuild: bool,
        /// Источник данных: claude (по умолчанию) | opencode
        #[arg(long, default_value = "claude")]
        source: String,
    },
    /// Сводка по всему индексу
    Stats,
    /// Топ прожорливых
    Top {
        /// task | agent | project | model
        #[arg(long, default_value = "task")]
        group: String,
        /// cost | tokens
        #[arg(long, default_value = "cost")]
        by: String,
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// Разбор одной задачи (promptId)
    Task {
        prompt_id: String,
    },
    /// Бенчмарк типа агента во времени
    Bench {
        agent_type: String,
    },
    /// Список поставленных задач (твоих промптов) с разбивкой
    Tasks {
        /// Фильтр по проекту (подстрока)
        #[arg(long)]
        project: Option<String>,
        /// time | cost
        #[arg(long, default_value = "time")]
        by: String,
        #[arg(long, default_value_t = 30)]
        limit: i64,
    },
    /// Цельная страница-отчёт (открывается в пейджере)
    Usage,
    /// Сгенерировать HTML-отчёт и открыть в браузере
    Report {
        /// Фильтр по проекту (подстрока) — отчёт только по нему
        #[arg(long)]
        project: Option<String>,
        /// Путь для файла (по умолчанию ~/.claude/quickai-report[-<project>].html)
        #[arg(long)]
        out: Option<String>,
        /// Не открывать браузер автоматически
        #[arg(long)]
        no_open: bool,
    },
    /// #1 Cache-hit по сессиям (кандидаты на фикс кэша)
    Cache {
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// #2 Латентность моделей + параллелизм субагентов
    Latency {
        #[arg(long, default_value_t = 15)]
        limit: i64,
    },
    /// #3 Спиннинг — задачи, кружащие впустую
    Spin {
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// #4 Профиль тулзов (вызовы + error-rate)
    Tools {
        #[arg(long, default_value_t = 25)]
        limit: i64,
    },
    /// #5 Waste — распределение stop_reason
    Waste,
    /// Долгие задачи по wall-времени + доля простоя (почему харнесс тормозил)
    Slow {
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// Запустить MCP-сервер (stdio) — те же запросы из диалога
    Mcp,
    /// Экспорт агрегатов в машинно-читаемый дамп для внешнего сбора
    Export {
        /// Фильтр по проекту (подстрока) — пусто = все проекты
        #[arg(long)]
        project: Option<String>,
        /// Форматировать JSON (по умолчанию — компактный одной строкой)
        #[arg(long)]
        pretty: bool,
        /// Формат вывода (пока только json; задел под будущие форматы)
        #[arg(long, value_enum, default_value_t = ExportFormat::Json)]
        format: ExportFormat,
    },
}

/// Формат экспортного дампа. Новые форматы добавляются сюда.
#[derive(Clone, Debug, ValueEnum)]
enum ExportFormat {
    Json,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Index { rebuild, source } => {
            let src = source::resolve(&source)?;
            let mut conn = index::open_db()?;
            let s = index::run(&mut conn, rebuild, src.as_ref())?;
            println!(
                "индекс готов [{}]: {} файлов просмотрено, {} проиндексировано, +{} turn'ов",
                src.name(),
                s.files_scanned,
                s.files_indexed,
                s.turns_added
            );
        }
        Cmd::Stats => {
            let conn = index::open_db()?;
            print!("{}", cli::render_stats(&query::stats(&conn, "")?));
        }
        Cmd::Top { group, by, limit } => {
            let conn = index::open_db()?;
            let rows = query::top(&conn, &group, &by, limit, "")?;
            print!("{}", cli::render_top(&rows, &group, &by));
        }
        Cmd::Task { prompt_id } => {
            let conn = index::open_db()?;
            print!("{}", cli::render_task(&query::task(&conn, &prompt_id)?));
        }
        Cmd::Bench { agent_type } => {
            let conn = index::open_db()?;
            print!("{}", cli::render_bench(&query::bench(&conn, &agent_type)?));
        }
        Cmd::Tasks { project, by, limit } => {
            let conn = index::open_db()?;
            let rows = query::tasks_list(&conn, project.as_deref(), &by, limit)?;
            let title = format!("Задачи ({} по {})", rows.len(), by);
            cli::page(&cli::render_tasks(&rows, &title));
        }
        Cmd::Usage => {
            let conn = index::open_db()?;
            let stats = query::stats(&conn, "")?;
            let models = query::top(&conn, "model", "cost", 8, "")?;
            let projects = query::top(&conn, "project", "cost", 10, "")?;
            let agents = query::top(&conn, "agenttype", "cost", 12, "")?;
            let tasks = query::tasks_list(&conn, None, "cost", 15)?;
            cli::page(&cli::render_usage(&stats, &models, &projects, &tasks, &agents));
        }
        Cmd::Report { project, out, no_open } => {
            use std::collections::HashMap;
            let conn = index::open_db()?;
            let p = project.as_deref().unwrap_or("");
            let stats = query::stats(&conn, p)?;
            let models = query::top(&conn, "model", "cost", 20, p)?;
            let projects = query::top(&conn, "project", "cost", 30, p)?;
            let agenttypes = query::top(&conn, "agenttype", "cost", 50, p)?;
            let tasks = query::tasks_list(&conn, project.as_deref(), "cost", 100_000)?;
            let mut agents: HashMap<String, Vec<query::AgentLine>> = HashMap::new();
            for (pid, line) in query::all_agent_runs(&conn, p)? {
                agents.entry(pid).or_default().push(line);
            }
            let extra = report::Extra {
                cache: query::cache_health(&conn, 15, p)?,
                models: query::model_latency(&conn, p)?,
                parallel: query::parallel_tasks(&conn, 15, p)?,
                spin: query::spinning(&conn, 20, p)?,
                tools: query::tool_profile(&conn, 30, p)?,
                waste: query::waste(&conn, p)?,
                slow: query::slow(&conn, 20, p)?,
            };
            let html = report::build(&stats, &models, &projects, &agenttypes, &tasks, &agents, &extra, p);

            let path = out.unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                let suffix = project.as_deref()
                    .map(|s| format!("-{}", s.replace('/', "_")))
                    .unwrap_or_default();
                format!("{home}/.claude/quickai-report{suffix}.html")
            });
            std::fs::write(&path, html)?;
            println!("отчёт: {path}");
            if !no_open {
                let _ = std::process::Command::new("open").arg(&path).status();
            }
        }
        Cmd::Cache { limit } => {
            let conn = index::open_db()?;
            print!("{}", cli::render_cache(&query::cache_health(&conn, limit, "")?));
        }
        Cmd::Latency { limit } => {
            let conn = index::open_db()?;
            let m = query::model_latency(&conn, "")?;
            let p = query::parallel_tasks(&conn, limit, "")?;
            print!("{}", cli::render_latency(&m, &p));
        }
        Cmd::Spin { limit } => {
            let conn = index::open_db()?;
            print!("{}", cli::render_spinning(&query::spinning(&conn, limit, "")?));
        }
        Cmd::Tools { limit } => {
            let conn = index::open_db()?;
            print!("{}", cli::render_tools(&query::tool_profile(&conn, limit, "")?));
        }
        Cmd::Waste => {
            let conn = index::open_db()?;
            print!("{}", cli::render_waste(&query::waste(&conn, "")?));
        }
        Cmd::Slow { limit } => {
            let conn = index::open_db()?;
            print!("{}", cli::render_slow(&query::slow(&conn, limit, "")?));
        }
        Cmd::Mcp => {
            mcp::serve()?;
        }
        Cmd::Export { project, pretty, format } => {
            let conn = index::open_db()?;
            match format {
                ExportFormat::Json => {
                    export::run(&conn, project.as_deref().unwrap_or(""), pretty)?;
                }
            }
        }
    }
    Ok(())
}
