//! Output-limit resolution: the most output tokens a request asks for,
//! chosen once from the caller's own value, the catalogue's declaration for
//! the model, or the provider adapter's account of the documented limit.
//!
//! The caller's value — a generation preset, or a bounded job's own cap —
//! always wins; a model's declared limit fills the request only where the
//! caller set none. The outcome names its source so the turn's record can
//! say where the number came from. A model no source declares resolves to
//! nothing, and the adapter's own floor applies on the wire.

use serde::{Deserialize, Serialize};

/// Where a resolved output limit came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputLimitSource {
    /// The request's own value: a generation preset, or a bounded job's cap.
    Caller,
    /// The catalogue entry's `capabilities.output_limit`.
    Catalogue,
    /// The provider adapter's account of the model's documented limit.
    Adapter,
}

/// The output limit a request carries, and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedOutputLimit {
    pub tokens: u32,
    pub source: OutputLimitSource,
}

/// Resolve the output limit a request carries: the caller's value, else the
/// catalogue's declaration, else the adapter's account; `None` when no
/// source declares one.
pub fn resolve_output_limit(
    caller: Option<u32>,
    catalogue: Option<u32>,
    adapter: Option<u32>,
) -> Option<ResolvedOutputLimit> {
    let (tokens, source) = caller
        .map(|tokens| (tokens, OutputLimitSource::Caller))
        .or_else(|| catalogue.map(|tokens| (tokens, OutputLimitSource::Catalogue)))
        .or_else(|| adapter.map(|tokens| (tokens, OutputLimitSource::Adapter)))?;
    Some(ResolvedOutputLimit { tokens, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row of the resolution table: caller, catalogue, adapter, outcome.
    type Case = (
        Option<u32>,
        Option<u32>,
        Option<u32>,
        Option<ResolvedOutputLimit>,
    );

    #[test]
    fn the_nearest_source_wins_and_an_undeclared_limit_resolves_to_nothing() {
        let cases: [Case; 5] = [
            (
                Some(512),
                Some(50_000),
                Some(128_000),
                Some(ResolvedOutputLimit {
                    tokens: 512,
                    source: OutputLimitSource::Caller,
                }),
            ),
            (
                None,
                Some(50_000),
                Some(128_000),
                Some(ResolvedOutputLimit {
                    tokens: 50_000,
                    source: OutputLimitSource::Catalogue,
                }),
            ),
            (
                None,
                None,
                Some(128_000),
                Some(ResolvedOutputLimit {
                    tokens: 128_000,
                    source: OutputLimitSource::Adapter,
                }),
            ),
            (
                Some(512),
                None,
                None,
                Some(ResolvedOutputLimit {
                    tokens: 512,
                    source: OutputLimitSource::Caller,
                }),
            ),
            (None, None, None, None),
        ];
        for (caller, catalogue, adapter, expected) in cases {
            assert_eq!(
                resolve_output_limit(caller, catalogue, adapter),
                expected,
                "caller {caller:?}, catalogue {catalogue:?}, adapter {adapter:?}"
            );
        }
    }

    #[test]
    fn the_record_names_its_source_in_snake_case() {
        let resolved = ResolvedOutputLimit {
            tokens: 128_000,
            source: OutputLimitSource::Adapter,
        };
        assert_eq!(
            serde_json::to_value(resolved).unwrap(),
            serde_json::json!({ "tokens": 128_000, "source": "adapter" })
        );
    }
}
