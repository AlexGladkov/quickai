# quickai — профайлер работы нейронки

Инструмент профилирования Claude Code: сколько токенов/денег/времени съела каждая
задача, сколько агентов вызвано, что потратил каждый субагент, бенчмарки во времени.

**Ничего не инструментируем — только парсим то, что Claude Code уже пишет на диск.**

---

## 1. Источник данных

```
~/.claude/projects/<project-slug>/
  <sessionId>.jsonl                        главный агент, turn-за-turn
  <sessionId>/subagents/agent-<id>.jsonl   каждый субагент отдельным файлом
  sessions-index.json
```

Масштаб (замер): ~2165 jsonl, ~1.3 GB, 31 проект. Нужен стриминговый парс.

### Что несёт каждый assistant-turn (`.message.usage`)
| Поле | Смысл |
|---|---|
| `input_tokens` | вход, полная цена |
| `output_tokens` | выход, 5× входа |
| `cache_creation.ephemeral_5m_input_tokens` | запись кэша 5m, 1.25× |
| `cache_creation.ephemeral_1h_input_tokens` | запись кэша 1h, 2× |
| `cache_read_input_tokens` | чтение кэша, 0.1× |
| `server_tool_use.web_search_requests` / `web_fetch_requests` | серверные тулзы |

### Идентификаторы для связывания
| Поле | Роль |
|---|---|
| `promptId` | **ключ задачи** — один пользовательский запрос |
| `sessionId` | сессия |
| `requestId` | один API-вызов |
| `uuid` / `parentUuid` | дерево turn'ов |
| `isSidechain` | признак turn'а субагента |
| `message.model` | модель (цена по-модельно) |
| `timestamp` | латентность |
| `cwd` / `gitBranch` / `version` | контекст |

Субагентский `agent-<id>.jsonl` несёт back-ref: тот же `sessionId` + `promptId`.
Тип агента (`subagent_type`) берётся из `Task` tool_use в главной сессии.

### Линковка субагент ↔ тип (РЕШЕНО, с лимитом данных)
`toolUseResult` строки-результата `Agent`/`Task` tool_use несёт `agentId` + `agentType`.
`agentId` == `<id>` из имени `agent-<id>.jsonl`. Матчим по нему (`agent_meta` таблица).

**Покрытие ~49%** на текущих данных. Несматченные `?`:
- старый формат транскриптов — результат без `agentId`;
- вложенные субагенты (субагент спавнит субагента) — результат не всегда с id.

Это лимит данных, не баг. `stats` показывает % покрытия явно. Возможный будущий
fallback: в пределах promptId матчить осиротевшие файлы к `subagent_type` из
`Agent` tool_use по порядку/количеству (фаззи).

---

## 2. Конвейер

```
INDEX   walk ~/.claude/projects/**/*.jsonl (стриминг, построчно)
          ↓ инкрементально: files(mtime,size,bytes_read) → дочитываем хвост
PARSE   turn → {promptId, sessionId, agentType, model, токены×5, ts, tools}
          ↓
PRICE   model → base-price × multiplier → cost_usd по каждому типу токена
          ↓
STORE   SQLite: turns → agent_runs → tasks → sessions → projects
          ↓
QUERY   CLI/MCP читают агрегаты. Пересчёт не нужен, БД = кэш.
```

БД — производная от jsonl (source of truth). Reindex идемпотентен. Смена прайса →
пересчёт `cost_usd` без репарса (цены хранятся отдельно, стоимость — денормализована).

---

## 3. Модель данных (SQLite)

```sql
files(                       -- инкрементальный индекс
  path TEXT PRIMARY KEY,
  mtime INTEGER, size INTEGER,
  bytes_read INTEGER,        -- офсет: дочитываем только хвост
  last_indexed INTEGER
)

sessions(
  session_id TEXT PRIMARY KEY,
  project TEXT, cwd TEXT, git_branch TEXT,
  first_ts INTEGER, last_ts INTEGER, version TEXT
)

tasks(                       -- один promptId = одна пользовательская задача
  prompt_id TEXT PRIMARY KEY,
  session_id TEXT, project TEXT,
  text TEXT,                 -- первый user-prompt (обрезанный)
  first_ts INTEGER, last_ts INTEGER,
  wall_ms INTEGER,           -- реальное время (last-first)
  cost_usd REAL, out_tokens INTEGER, agent_count INTEGER
)

agent_runs(                  -- одна инвокация субагента
  id TEXT PRIMARY KEY,       -- agent-<id>
  prompt_id TEXT, session_id TEXT,
  agent_type TEXT,           -- swift-expert / Explore / ...
  file_path TEXT,
  first_ts INTEGER, last_ts INTEGER,
  turns INTEGER, cost_usd REAL, out_tokens INTEGER
)

turns(                       -- один assistant-turn (главный или сайдчейн)
  id INTEGER PRIMARY KEY,
  prompt_id TEXT, session_id TEXT,
  agent_run_id TEXT,         -- NULL = главный агент
  is_sidechain INTEGER,
  model TEXT, ts INTEGER,
  input_tokens INTEGER, output_tokens INTEGER,
  cache_write_5m INTEGER, cache_write_1h INTEGER, cache_read INTEGER,
  web_search INTEGER, web_fetch INTEGER,
  cost_usd REAL
)

-- индексы: turns(prompt_id), turns(agent_run_id), agent_runs(prompt_id),
--          tasks(project), turns(model), *(ts)
```

Стоимость считается на этапе INDEX и денормализуется вверх (turn → agent_run → task).

---

## 4. Прайсинг (`pricing/`)

Источник цен — справочник claude-api. Base-price × multiplier, правится одной строкой.

| Модель | input $/1M | output $/1M |
|---|---|---|
| `claude-opus-4-8` | 5.00 | 25.00 |
| `claude-sonnet-4-6` | 3.00 | 15.00 |
| `claude-haiku-4-5` | 1.00 | 5.00 |

Множители (input = 1×): output 5×, cache-write-5m 1.25×, cache-write-1h 2×, cache-read 0.1×.

```
cost = (input·1 + output·5 + cw5m·1.25 + cw1h·2 + cr·0.1) / 1e6 × base_input_price
```
где `output` уже включает свой множитель через отдельную ставку — фактически:
```
cost = ( input·Pin + output·Pout
       + cw5m·Pin·1.25 + cw1h·Pin·2 + cr·Pin·0.1 ) / 1e6
```
Fallback на неизвестную модель → цены Opus (не занижать). Override — `pricing.toml`.

---

## 5. CLI

```
quickai index [--rebuild]            построить/обновить индекс
quickai task <promptId>              разбор одной задачи (дерево агентов)
quickai top --by cost|tokens         топ прожорливых
           --group task|agent|project|model
           --period 7d|30d|all
quickai bench <agentType>            бенчмарк агента во времени
quickai session <id>                 разбор сессии
quickai stats                        сводка по всей БД
quickai --mcp                        (позже) MCP-сервер над той же БД
```

Вывод: таблица (по умолчанию) или `--json`.

---

## 6. Раскладка кода

```
quickai/
  Cargo.toml
  ARCHITECTURE.md
  src/
    main.rs           clap entry, роутинг подкоманд
    model.rs          домен: Turn, AgentRun, Task, Session, Usage
    parse/
      mod.rs          стриминговый reader jsonl
      record.rs       serde-схема сырой строки
      linkage.rs      резолвер agent-file ↔ subagent_type
    index/
      mod.rs          инкрементальный индексатор
      schema.rs       DDL + миграции
    pricing.rs        model→price, cost calc
    query.rs          агрегирующие запросы
    cli/
      mod.rs
      task.rs top.rs bench.rs session.rs stats.rs
    mcp.rs            (позже) фасад над query
```

Зависимости: `clap`, `serde`, `serde_json`, `rusqlite` (bundled), `anyhow`,
`walkdir`, `chrono`.

---

## 7. Этапы

1. ✅ **Ядро** — INDEX + SQLite + `task`/`top`/`stats`. 1.3 GB за ~13с, инкрементально.
2. ✅ Линковка субагентов (`toolUseResult.agentId↔agentType`) — ~49%, лимит данных.
3. ✅ `bench` + дневной тренд; `top --group agenttype`; текст задачи в `top --group task`.
4. ✅ MCP-фасад (`quickai mcp`, stdio JSON-RPC) — 4 тула.
5. (опц.) live-хуки → statusline realtime.
6. (опц.) поднять покрытие линковки фаззи-fallback'ом.
