//! A runtime settings section declared once.
//!
//! One `settings_section!` invocation names a section's TOML table, its
//! fields with their defaults, and the semantic rules its values must meet,
//! and emits everything the cascade needs from that: the section struct
//! (the wire type and the resolved config type are one definition), its
//! file-layer partial with overlay and resolve, the field schema the config
//! file walker checks unknown fields and wrong types against, and the
//! semantic check both the file loader and the settings API run. Adding a
//! section is the declaration plus one line at each registry (the resolved
//! config, the partial config, the walker's list, the settings response and
//! request, the hot-apply) — never a restatement of its fields.

use std::fmt;

/// The TOML kind a declared field holds, checked by the config file walker
/// before the typed deserialisation runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFieldKind {
    Integer,
    Boolean,
    String,
    Float,
    Array,
}

impl SettingsFieldKind {
    /// The name the walker reports in a type problem.
    pub fn name(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Boolean => "boolean",
            Self::String => "string",
            Self::Float => "float",
            Self::Array => "array",
        }
    }
}

/// A Rust field type's TOML kind. Implemented for the scalar and list types a
/// declared section may carry; a field of another type is a compile error
/// at the declaration, which is where it should be caught.
pub trait SettingsField {
    const KIND: SettingsFieldKind;
}

macro_rules! scalar_settings_fields {
    ($($ty:ty => $kind:ident),+ $(,)?) => {
        $(impl SettingsField for $ty {
            const KIND: SettingsFieldKind = SettingsFieldKind::$kind;
        })+
    };
}

scalar_settings_fields! {
    u16 => Integer,
    u32 => Integer,
    u64 => Integer,
    i32 => Integer,
    i64 => Integer,
    bool => Boolean,
    String => String,
    f64 => Float,
}

impl SettingsField for Vec<String> {
    const KIND: SettingsFieldKind = SettingsFieldKind::Array;
}

/// One declared field, as the config file walker reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsFieldSchema {
    pub name: &'static str,
    pub kind: SettingsFieldKind,
}

/// A declared section's table name and fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsSectionSchema {
    pub name: &'static str,
    pub fields: &'static [SettingsFieldSchema],
}

impl SettingsSectionSchema {
    pub fn field(&self, name: &str) -> Option<&SettingsFieldSchema> {
        self.fields.iter().find(|field| field.name == name)
    }
}

/// A value a declared section refuses, named by the field that carries it so
/// the file loader can point at `section.field`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsProblem {
    pub field: &'static str,
    pub message: String,
}

impl fmt::Display for SettingsProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// What every declared section provides: its schema, its partial, and the
/// semantic check its values must pass wherever they arrive — the config
/// file, the settings API, a programmatic overlay.
pub trait SettingsSection: Sized + Default {
    /// The file-layer mirror: every field optional, overlaid child-wins
    /// across the config hierarchy and resolved against the defaults.
    type Partial: Default;

    const SCHEMA: SettingsSectionSchema;

    /// Resolve a partial to a complete section, defaults filling the gaps.
    fn resolve_partial(partial: Self::Partial) -> Self;

    /// The values this section refuses. Empty means acceptable.
    fn problems(&self) -> Vec<SettingsProblem>;
}

/// Declare a runtime settings section once.
///
/// ```ignore
/// settings_section! {
///     /// Session list housekeeping.
///     section "sessions" as SessionsConfig / PartialSessionsConfig {
///         /// Days without a message before a session leaves the recent list.
///         #[ts(type = "number")]
///         archive_after_days: u64 = 7,
///     }
///     problems(section) {
///         let mut problems = Vec::new();
///         // push a SettingsProblem { field, message } for each refused value
///         problems
///     }
/// }
/// ```
///
/// Field attributes are applied to the section struct only (the partial
/// derives no projection, so a `#[ts(...)]` attribute has nothing to say
/// there). Field types are those `SettingsField` is implemented for.
#[macro_export]
macro_rules! settings_section {
    (
        $(#[$attr:meta])*
        section $table:literal as $name:ident / $partial:ident {
            $(
                $(#[$fattr:meta])*
                $field:ident : $ty:ty = $default:expr
            ),+ $(,)?
        }
        problems($this:ident) $problems:block
    ) => {
        $(#[$attr])*
        #[derive(
            Debug, Clone, PartialEq, ::serde::Serialize, ::serde::Deserialize,
            ::schemars::JsonSchema, ::ts_rs::TS,
        )]
        #[serde(deny_unknown_fields)]
        #[ts(optional_fields = nullable)]
        pub struct $name {
            $(
                $(#[$fattr])*
                pub $field: $ty,
            )+
        }

        impl Default for $name {
            fn default() -> Self {
                Self {
                    $($field: $default,)+
                }
            }
        }

        #[doc = concat!("The file-layer mirror of [`", stringify!($name), "`]: every field optional.")]
        #[derive(Debug, Clone, Default, PartialEq, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $partial {
            $(pub $field: Option<$ty>,)+
        }

        impl From<$name> for $partial {
            fn from(section: $name) -> Self {
                Self {
                    $($field: Some(section.$field),)+
                }
            }
        }

        impl $partial {
            /// Child-wins overlay: a field the overlay sets replaces the base's.
            pub fn overlay(self, overlay: Self) -> Self {
                Self {
                    $($field: overlay.$field.or(self.$field),)+
                }
            }

            /// The complete section, defaults filling what no layer set.
            pub fn resolve(self) -> $name {
                $name {
                    $($field: self.$field.unwrap_or($default),)+
                }
            }
        }

        impl $crate::client::settings_section::SettingsSection for $name {
            type Partial = $partial;

            const SCHEMA: $crate::client::settings_section::SettingsSectionSchema =
                $crate::client::settings_section::SettingsSectionSchema {
                    name: $table,
                    fields: &[
                        $(
                            $crate::client::settings_section::SettingsFieldSchema {
                                name: stringify!($field),
                                kind: <$ty as $crate::client::settings_section::SettingsField>::KIND,
                            },
                        )+
                    ],
                };

            fn resolve_partial(partial: Self::Partial) -> Self {
                partial.resolve()
            }

            fn problems(&self) -> Vec<$crate::client::settings_section::SettingsProblem> {
                let $this = self;
                $problems
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    settings_section! {
        /// A section for the tests alone.
        section "probe" as ProbeConfig / PartialProbeConfig {
            /// How many.
            #[ts(type = "number")]
            count: u64 = 3,
            /// Whether.
            enabled: bool = true,
            /// A name.
            label: String = String::from("probe"),
        }
        problems(section) {
            let mut problems = Vec::new();
            if section.count == 0 {
                problems.push(SettingsProblem {
                    field: "count",
                    message: "probe.count must be positive".into(),
                });
            }
            problems
        }
    }

    #[test]
    fn a_declaration_yields_its_schema_defaults_and_partial() {
        assert_eq!(ProbeConfig::SCHEMA.name, "probe");
        assert_eq!(
            ProbeConfig::SCHEMA
                .fields
                .iter()
                .map(|field| (field.name, field.kind))
                .collect::<Vec<_>>(),
            vec![
                ("count", SettingsFieldKind::Integer),
                ("enabled", SettingsFieldKind::Boolean),
                ("label", SettingsFieldKind::String),
            ]
        );
        let defaults = ProbeConfig::default();
        assert_eq!(
            (defaults.count, defaults.enabled, defaults.label.as_str()),
            (3, true, "probe")
        );
        assert_eq!(PartialProbeConfig::default().resolve(), defaults);
    }

    #[test]
    fn the_partial_overlays_child_wins_and_resolves_against_defaults() {
        let base = PartialProbeConfig {
            count: Some(5),
            enabled: Some(false),
            label: None,
        };
        let overlay = PartialProbeConfig {
            count: Some(9),
            enabled: None,
            label: None,
        };
        let merged = base.overlay(overlay);
        assert_eq!(merged.count, Some(9), "the overlay's value wins");
        assert_eq!(
            merged.enabled,
            Some(false),
            "the base survives where the overlay is silent"
        );
        let resolved = merged.resolve();
        assert_eq!(
            (resolved.count, resolved.enabled, resolved.label.as_str()),
            (9, false, "probe")
        );
    }

    #[test]
    fn a_section_round_trips_into_its_partial_and_back() {
        let section = ProbeConfig {
            count: 1,
            enabled: false,
            label: "x".into(),
        };
        let partial: PartialProbeConfig = section.clone().into();
        assert_eq!(partial.resolve(), section);
    }

    #[test]
    fn the_semantic_check_names_the_field_it_refuses() {
        let problems = ProbeConfig {
            count: 0,
            ..ProbeConfig::default()
        }
        .problems();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].field, "count");
        assert!(ProbeConfig::default().problems().is_empty());
    }

    #[test]
    fn the_wire_shape_refuses_an_unknown_field_and_the_partial_does_too() {
        let unknown = serde_json::json!({ "count": 1, "enabled": true, "label": "x", "extra": 1 });
        assert!(serde_json::from_value::<ProbeConfig>(unknown.clone()).is_err());
        assert!(serde_json::from_value::<PartialProbeConfig>(unknown).is_err());
        let sparse: PartialProbeConfig =
            serde_json::from_value(serde_json::json!({ "count": 4 })).unwrap();
        assert_eq!(sparse.count, Some(4));
        assert_eq!(sparse.label, None);
    }
    settings_section! {
        /// List field probe.
        section "list_probe" as ListProbeConfig / PartialListProbeConfig {
            values: Vec<String> = vec!["default".into()],
        }
        problems(_section) { Vec::new() }
    }

    #[test]
    fn a_list_field_keeps_its_shape_defaults_and_child_wins_overlay() {
        assert_eq!(ListProbeConfig::SCHEMA.fields[0].kind.name(), "array");
        assert_eq!(
            PartialListProbeConfig::default().resolve().values,
            ["default"]
        );
        let base: PartialListProbeConfig =
            serde_json::from_value(serde_json::json!({"values": ["base", "other"]})).unwrap();
        let child: PartialListProbeConfig =
            serde_json::from_value(serde_json::json!({"values": ["child"]})).unwrap();
        let resolved = base.overlay(child).resolve();
        assert_eq!(resolved.values, ["child"]);
        let partial: PartialListProbeConfig = resolved.clone().into();
        assert_eq!(partial.resolve(), resolved);
        assert!(
            serde_json::from_value::<PartialListProbeConfig>(serde_json::json!({"values": [42]}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<ListProbeConfig>(
                serde_json::json!({"values": [], "extra": 1})
            )
            .is_err()
        );
    }
}
