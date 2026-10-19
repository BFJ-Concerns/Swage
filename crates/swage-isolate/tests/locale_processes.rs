use std::process::Command;
use std::time::Duration;

use deno_core::{JsRuntime, RuntimeOptions, v8};
use serde_json::Value;
use swage_isolate::{IsolateLimits, Program};

const RECORD: &str = "LOCALE_RECORD=";
const LOCALES: [&str; 4] = ["en_US.UTF-8", "de_DE.UTF-8", "tr_TR.UTF-8", "sv_SE.UTF-8"];
const ARGUMENT_LOCALES: [&str; 5] = ["default", "en-US", "de-DE", "tr-TR", "sv-SE"];
const METHODS: [&str; 7] = [
    "Number.prototype.toLocaleString",
    "BigInt.prototype.toLocaleString",
    "Array.prototype.toLocaleString",
    "TypedArray.prototype.toLocaleString",
    "String.prototype.localeCompare",
    "String.prototype.toLocaleLowerCase",
    "String.prototype.toLocaleUpperCase",
];

// The same calls run both through the real cage and in an uncaged control.
// Explicit locales cover rendering, Turkish case mapping and Swedish collation.
const CALLS: &str = r#"(() => {
    const methods = [
        ["Number.prototype.toLocaleString", (...args) => (1234567.89).toLocaleString(...args)],
        ["BigInt.prototype.toLocaleString", (...args) => 123456789n.toLocaleString(...args)],
        ["Array.prototype.toLocaleString", (...args) => [1234567.89, 9876.5].toLocaleString(...args)],
        ["TypedArray.prototype.toLocaleString", (...args) => new Float64Array([1234567.89, 9876.5]).toLocaleString(...args)],
        ["String.prototype.localeCompare", (...args) => "ä".localeCompare("z", ...args)],
        ["String.prototype.toLocaleLowerCase", (...args) => "I".toLocaleLowerCase(...args)],
        ["String.prototype.toLocaleUpperCase", (...args) => "i".toLocaleUpperCase(...args)],
    ];
    return methods.flatMap(([method, call]) =>
        [undefined, "en-US", "de-DE", "tr-TR", "sv-SE"].map(locale => {
            let outcome;
            try { outcome = { value: locale === undefined ? call() : call(locale) }; }
            catch (error) { outcome = { error: { name: error.name, message: error.message } }; }
            return { method, locale: locale ?? "default", outcome };
        })
    );
})()"#;

/// Re-invoked directly, including under nextest: no shell, inherited locale,
/// operator configuration or parent-process ICU cache reaches these children.
#[test]
fn caged_locale_methods_agree_across_effective_default_locales() {
    let records: Vec<Value> = LOCALES
        .iter()
        .map(|locale| {
            let output = Command::new(std::env::current_exe().expect("test executable"))
                .args(["--ignored", "--exact", "locale_child", "--nocapture"])
                .env_clear()
                .env("LANG", locale)
                .env("LC_ALL", locale)
                .env("LC_MESSAGES", locale)
                .env("LC_CTYPE", locale)
                .output()
                .expect("run locale child");
            assert!(
                output.status.success(),
                "{locale}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8(output.stdout).expect("UTF-8 child output");
            let record = stdout
                .lines()
                .find_map(|line| line.strip_prefix(RECORD))
                .expect("child emitted its result record");
            println!("{locale}: {record}");
            serde_json::from_str(record).expect("JSON child record")
        })
        .collect();

    // Environment labels alone prove nothing. Require the live engine to
    // resolve all four defaults and demonstrate locale-sensitive values.
    for (record, language) in records.iter().zip(["en-US", "de-DE", "tr-TR", "sv-SE"]) {
        assert_eq!(record["defaultLocale"], language, "{record}");
    }
    for (method_index, method) in METHODS.iter().enumerate() {
        let index = method_index * ARGUMENT_LOCALES.len();
        assert!(
            records.iter().all(|record| {
                record["uncaged"].as_array().expect("uncaged outcomes")
                    [index..index + ARGUMENT_LOCALES.len()]
                    .iter()
                    .all(|entry| entry["outcome"].get("value").is_some())
            }),
            "uncaged control must return a value for {method}"
        );
        if method.starts_with("String.prototype.toLocale") {
            // V8's omitted-locale case-mapping fast path can use root case
            // mapping even with a Turkish default. Explicit Turkish must
            // still distinguish these inputs from explicit English.
            for record in &records {
                assert_ne!(
                    record["uncaged"][index + 1]["outcome"],
                    record["uncaged"][index + 3]["outcome"],
                    "explicit locales did not discriminate {method}"
                );
            }
        } else {
            assert!(
                records[1..].iter().any(|record| {
                    record["uncaged"][index]["outcome"] != records[0]["uncaged"][index]["outcome"]
                }),
                "effective default locales did not discriminate {method}"
            );
        }
    }

    for record in &records {
        let outcomes = record["caged"].as_array().expect("caged outcomes");
        assert_eq!(outcomes.len(), METHODS.len() * ARGUMENT_LOCALES.len());
        for (method_index, method) in METHODS.iter().enumerate() {
            for (locale_index, locale) in ARGUMENT_LOCALES.iter().enumerate() {
                let entry = &outcomes[method_index * ARGUMENT_LOCALES.len() + locale_index];
                assert_eq!(entry["method"], *method);
                assert_eq!(entry["locale"], *locale);
                let outcome = &entry["outcome"];
                if let Some(error) = outcome.get("error") {
                    assert_eq!(error["name"], "NondeterministicCallError", "{entry}");
                    assert!(
                        error["message"]
                            .as_str()
                            .expect("error message")
                            .starts_with(&format!("{method} is not available")),
                        "{entry}"
                    );
                } else {
                    assert!(outcome.get("value").is_some(), "{entry}");
                }
            }
        }
    }
    for (record, locale) in records[1..].iter().zip(&LOCALES[1..]) {
        assert_eq!(
            records[0]["caged"], record["caged"],
            "caged outputs differ between {} and {locale}",
            LOCALES[0]
        );
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "subprocess helper; invoked by the locale journey"]
async fn locale_child() {
    let mut program = Program::new(
        IsolateLimits {
            heap_bytes: 16 * 1024 * 1024,
            continuous_execution: Duration::from_secs(5),
        },
        None,
        None,
    )
    .expect("real caged isolate");
    let caged = program
        .execute_for_string("locale-proof", format!("JSON.stringify({CALLS})"))
        .expect("caged locale calls");
    drop(program);

    // Program initialises the shared V8 platform. This separate realm retains
    // Intl and the original methods solely to validate the environment.
    let mut uncaged = JsRuntime::new(RuntimeOptions::default());
    let control = uncaged
        .execute_script(
            "locale-control",
            format!(
                "JSON.stringify({{ defaultLocale: new Intl.NumberFormat().resolvedOptions().locale, outcomes: {CALLS} }})"
            ),
        )
        .expect("uncaged locale control");
    deno_core::scope!(scope, uncaged);
    let control = v8::Local::new(scope, control);
    let control = v8::Local::<v8::String>::try_from(control)
        .expect("control JSON string")
        .to_rust_string_lossy(scope);
    let control: Value = serde_json::from_str(&control).expect("control JSON");
    let caged: Value = serde_json::from_str(&caged).expect("caged JSON");
    println!(
        "{RECORD}{}",
        serde_json::json!({
            "defaultLocale": control["defaultLocale"],
            "uncaged": control["outcomes"],
            "caged": caged,
        })
    );
}
