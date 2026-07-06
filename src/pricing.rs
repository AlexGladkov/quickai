//! Прайсинг моделей. Источник цен — справочник claude-api.
//! Base input-price × множитель. Правится одной строкой на модель.

use crate::model::Usage;

/// Цена входа/выхода за 1M токенов, $.
#[derive(Debug, Clone, Copy)]
pub struct Price {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

// Множители относительно input-цены (кэш) — общие для всех моделей.
const MUL_CACHE_WRITE_5M: f64 = 1.25;
const MUL_CACHE_WRITE_1H: f64 = 2.0;
const MUL_CACHE_READ: f64 = 0.10;

/// Таблица цен (актуально на 2026-07, claude-api). Fallback → Opus.
pub fn price_for(model: &str) -> Price {
    match model {
        m if m.contains("opus") => Price { input_per_mtok: 5.0, output_per_mtok: 25.0 },
        m if m.contains("sonnet") => Price { input_per_mtok: 3.0, output_per_mtok: 15.0 },
        m if m.contains("haiku") => Price { input_per_mtok: 1.0, output_per_mtok: 5.0 },
        m if m.contains("fable") => Price { input_per_mtok: 10.0, output_per_mtok: 50.0 },
        // Неизвестная модель → не занижаем, считаем по Opus.
        _ => Price { input_per_mtok: 5.0, output_per_mtok: 25.0 },
    }
}

/// Стоимость одного turn'а, $. Кэш считается от input-ставки через множители.
pub fn cost_of(model: &str, u: &Usage) -> f64 {
    let p = price_for(model);
    let pin = p.input_per_mtok;
    let dollars = u.input as f64 * pin
        + u.output as f64 * p.output_per_mtok
        + u.cache_write_5m as f64 * pin * MUL_CACHE_WRITE_5M
        + u.cache_write_1h as f64 * pin * MUL_CACHE_WRITE_1H
        + u.cache_read as f64 * pin * MUL_CACHE_READ;
    dollars / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_basic() {
        let u = Usage { input: 1_000_000, output: 1_000_000, ..Default::default() };
        // 1M input @ $5 + 1M output @ $25 = $30
        assert!((cost_of("claude-opus-4-8", &u) - 30.0).abs() < 1e-9);
    }

    #[test]
    fn cache_read_cheap() {
        let u = Usage { cache_read: 1_000_000, ..Default::default() };
        // 1M cache-read @ 0.1× × $5 = $0.5
        assert!((cost_of("claude-opus-4-8", &u) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn unknown_falls_back_to_opus() {
        let u = Usage { input: 1_000_000, ..Default::default() };
        assert!((cost_of("claude-mystery-9", &u) - 5.0).abs() < 1e-9);
    }
}
