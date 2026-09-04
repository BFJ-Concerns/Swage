//! Effort resolution: one provider-neutral intent becomes the control a
//! particular model actually accepts, with the outcome recorded.
//!
//! The user chooses an [`Effort`]; the model declares an
//! [`EffortCapability`]; [`resolve_effort`] is the only place the two meet.
//! Every provider adapter consumes the resolved control, never the intent,
//! so a clamp or a substitution happens once and is written into the audit
//! trail rather than re-derived — and possibly differently — per adapter.

use serde::{Deserialize, Serialize};

pub use swage_protocol::client::settings::{Effort, EffortCapability};

/// The control a provider adapter puts on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffortControl {
    /// A named level the model accepts natively.
    Level { level: Effort },
    /// A numeric thinking budget, for a model whose control is a budget.
    Budget { tokens: u32 },
    /// Reasoning switched off through the model's own disable.
    Disabled,
    /// No control sent: the model declares none.
    Omitted,
}

/// The outcome of resolving an intent against a capability — what was
/// asked, what is sent, and whether the two differ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedEffort {
    /// The intent as chosen.
    pub requested: Effort,
    /// The control the adapter sends.
    pub control: EffortControl,
    /// When the control expresses a different level from the request —
    /// a clamp, or `off` on a model that cannot disable — the level the
    /// control stands for; `None` when the request was honoured as is.
    pub clamped_to: Option<Effort>,
    /// A one-line account of any substitution, for the record.
    pub note: Option<String>,
}

impl ResolvedEffort {
    fn honoured(requested: Effort, control: EffortControl) -> Self {
        Self {
            requested,
            control,
            clamped_to: None,
            note: None,
        }
    }

    fn substituted(
        requested: Effort,
        control: EffortControl,
        clamped_to: Effort,
        note: String,
    ) -> Self {
        Self {
            requested,
            control,
            clamped_to: Some(clamped_to),
            note: Some(note),
        }
    }
}

/// Resolve an intent against a model's declared capability.
///
/// - A level the model expresses natively is sent as that level.
/// - A level it lacks clamps to the nearest level it supports, downward
///   where one exists below the request and to the lowest level otherwise;
///   the clamp is recorded.
/// - `off` sends the model's own disable where it has one, and otherwise
///   clamps to the lowest supported level with "off unsupported" recorded.
/// - A budget-only model receives its native budget, spread monotonically
///   across its declared range so a higher intent never resolves to a
///   smaller budget.
/// - A model declaring no control has the control omitted.
pub fn resolve_effort(requested: Effort, capability: &EffortCapability) -> ResolvedEffort {
    match capability {
        EffortCapability::None => ResolvedEffort {
            requested,
            control: EffortControl::Omitted,
            clamped_to: None,
            note: Some("model declares no effort control; omitted".into()),
        },
        EffortCapability::Levels {
            levels,
            can_disable,
        } => {
            let mut supported: Vec<Effort> = levels
                .iter()
                .copied()
                .filter(|level| *level != Effort::Off)
                .collect();
            supported.sort();
            supported.dedup();
            let lowest = supported
                .first()
                .copied()
                .expect("a validated Levels capability declares at least one level");
            if requested == Effort::Off {
                return if *can_disable {
                    ResolvedEffort::honoured(requested, EffortControl::Disabled)
                } else {
                    ResolvedEffort::substituted(
                        requested,
                        EffortControl::Level { level: lowest },
                        lowest,
                        format!("off unsupported; clamped to {lowest}"),
                    )
                };
            }
            if supported.contains(&requested) {
                return ResolvedEffort::honoured(
                    requested,
                    EffortControl::Level { level: requested },
                );
            }
            let nearest = supported
                .iter()
                .rev()
                .find(|level| **level < requested)
                .copied()
                .unwrap_or(lowest);
            ResolvedEffort::substituted(
                requested,
                EffortControl::Level { level: nearest },
                nearest,
                format!("{requested} unsupported; clamped to {nearest}"),
            )
        }
        EffortCapability::Budget {
            min_tokens,
            max_tokens,
            can_disable,
        } => {
            if requested == Effort::Off {
                return if *can_disable {
                    ResolvedEffort::honoured(requested, EffortControl::Disabled)
                } else {
                    ResolvedEffort::substituted(
                        requested,
                        EffortControl::Budget {
                            tokens: *min_tokens,
                        },
                        Effort::Minimal,
                        format!("off unsupported; budget {min_tokens} tokens (minimal)"),
                    )
                };
            }
            // Six non-off levels spread across the range; the last always
            // lands on max_tokens and the first on min_tokens.
            let step = (requested as u32).saturating_sub(1);
            let span = u64::from(max_tokens.saturating_sub(*min_tokens));
            let tokens = min_tokens + u32::try_from(span * u64::from(step) / 5).unwrap_or(u32::MAX);
            ResolvedEffort::honoured(requested, EffortControl::Budget { tokens })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(levels: &[Effort], can_disable: bool) -> EffortCapability {
        EffortCapability::Levels {
            levels: levels.to_vec(),
            can_disable,
        }
    }

    #[test]
    fn a_native_level_is_sent_as_itself() {
        let resolved = resolve_effort(
            Effort::High,
            &levels(&[Effort::Low, Effort::Medium, Effort::High], true),
        );
        assert_eq!(
            resolved.control,
            EffortControl::Level {
                level: Effort::High
            }
        );
        assert_eq!(resolved.clamped_to, None);
        assert_eq!(resolved.note, None);
    }

    #[test]
    fn an_unsupported_level_clamps_down_and_is_recorded() {
        let resolved = resolve_effort(
            Effort::Max,
            &levels(
                &[Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh],
                true,
            ),
        );
        assert_eq!(
            resolved.control,
            EffortControl::Level {
                level: Effort::Xhigh
            }
        );
        assert_eq!(resolved.clamped_to, Some(Effort::Xhigh));
        assert_eq!(
            resolved.note.as_deref(),
            Some("max unsupported; clamped to xhigh")
        );
    }

    #[test]
    fn a_level_below_the_lowest_supported_takes_the_lowest() {
        let resolved = resolve_effort(
            Effort::Minimal,
            &levels(&[Effort::Low, Effort::High], false),
        );
        assert_eq!(
            resolved.control,
            EffortControl::Level { level: Effort::Low }
        );
        assert_eq!(resolved.clamped_to, Some(Effort::Low));
    }

    #[test]
    fn off_disables_where_possible_and_clamps_honestly_otherwise() {
        let disabled = resolve_effort(Effort::Off, &levels(&[Effort::Low, Effort::High], true));
        assert_eq!(disabled.control, EffortControl::Disabled);
        assert_eq!(disabled.note, None);

        let clamped = resolve_effort(Effort::Off, &levels(&[Effort::Low, Effort::High], false));
        assert_eq!(clamped.control, EffortControl::Level { level: Effort::Low });
        assert_eq!(
            clamped.note.as_deref(),
            Some("off unsupported; clamped to low")
        );
    }

    #[test]
    fn a_budget_model_receives_a_monotone_budget_across_its_range() {
        let capability = EffortCapability::Budget {
            min_tokens: 1000,
            max_tokens: 6000,
            can_disable: false,
        };
        let budgets: Vec<u32> = Effort::ALL
            .iter()
            .map(|level| match resolve_effort(*level, &capability).control {
                EffortControl::Budget { tokens } => tokens,
                other => panic!("budget model resolved to {other:?}"),
            })
            .collect();
        assert_eq!(budgets, [1000, 1000, 2000, 3000, 4000, 5000, 6000]);
        assert!(
            budgets.windows(2).all(|pair| pair[0] <= pair[1]),
            "a higher intent never resolves to a smaller budget"
        );
        let off = resolve_effort(Effort::Off, &capability);
        assert_eq!(off.clamped_to, Some(Effort::Minimal));
        assert!(off.note.as_deref().unwrap().starts_with("off unsupported"));
    }

    #[test]
    fn a_model_without_a_control_omits_it_and_says_so() {
        let resolved = resolve_effort(Effort::High, &EffortCapability::None);
        assert_eq!(resolved.control, EffortControl::Omitted);
        assert!(
            resolved
                .note
                .as_deref()
                .unwrap()
                .contains("no effort control")
        );
    }

    #[test]
    fn resolution_is_monotone_over_named_levels() {
        let capability = levels(&[Effort::Low, Effort::High, Effort::Max], true);
        let sent: Vec<Option<Effort>> = Effort::ALL
            .iter()
            .map(|level| match resolve_effort(*level, &capability).control {
                EffortControl::Level { level } => Some(level),
                EffortControl::Disabled => None,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            sent,
            [
                None,
                Some(Effort::Low),
                Some(Effort::Low),
                Some(Effort::Low),
                Some(Effort::High),
                Some(Effort::High),
                Some(Effort::Max)
            ]
        );
    }
}
