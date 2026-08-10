use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

use crate::EffectKind;

#[async_trait]
pub trait EffectRecoveryHost: Send + Sync {
    async fn resend(&self, key: Uuid, request: &Value) -> Result<Value, String>;
    async fn probe(&self, key: Uuid, request: &Value) -> Result<Option<Value>, String>;
}

pub async fn recover_started_effect(
    kind: EffectKind,
    key: Uuid,
    request: &Value,
    host: &dyn EffectRecoveryHost,
) -> Result<Value, String> {
    match kind {
        EffectKind::IdempotencyKey => host.resend(key, request).await,
        EffectKind::CheckBeforeRedo => match host.probe(key, request).await? {
            Some(receipt) => Ok(receipt),
            None => host.resend(key, request).await,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct Fake {
        probe_result: Option<Value>,
        probe_error: Option<String>,
        resend_error: Option<String>,
        probes: AtomicUsize,
        sends: AtomicUsize,
    }

    #[async_trait]
    impl EffectRecoveryHost for Fake {
        async fn resend(&self, _: Uuid, _: &Value) -> Result<Value, String> {
            self.sends.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = &self.resend_error {
                return Err(error.clone());
            }
            Ok(serde_json::json!({ "sent": true }))
        }
        async fn probe(&self, _: Uuid, _: &Value) -> Result<Option<Value>, String> {
            self.probes.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = &self.probe_error {
                return Err(error.clone());
            }
            Ok(self.probe_result.clone())
        }
    }

    impl Fake {
        fn returning(probe_result: Option<Value>) -> Self {
            Self {
                probe_result,
                probe_error: None,
                resend_error: None,
                probes: AtomicUsize::new(0),
                sends: AtomicUsize::new(0),
            }
        }
    }

    #[tokio::test]
    async fn idempotency_key_resends_without_probing() {
        let host = Fake::returning(None);

        let receipt = recover_started_effect(
            EffectKind::IdempotencyKey,
            Uuid::new_v4(),
            &Value::Null,
            &host,
        )
        .await
        .expect("recover");

        assert_eq!(receipt, serde_json::json!({ "sent": true }));
        assert_eq!(host.probes.load(Ordering::SeqCst), 0);
        assert_eq!(host.sends.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn check_before_redo_skips_when_probe_finds_effect() {
        let host = Fake::returning(Some(serde_json::json!({ "found": true })));
        recover_started_effect(
            EffectKind::CheckBeforeRedo,
            Uuid::new_v4(),
            &Value::Null,
            &host,
        )
        .await
        .expect("recover");
        assert_eq!(host.probes.load(Ordering::SeqCst), 1);
        assert_eq!(host.sends.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn check_before_redo_resends_when_probe_misses() {
        let host = Fake::returning(None);
        recover_started_effect(
            EffectKind::CheckBeforeRedo,
            Uuid::new_v4(),
            &Value::Null,
            &host,
        )
        .await
        .expect("recover");
        assert_eq!(host.probes.load(Ordering::SeqCst), 1);
        assert_eq!(host.sends.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn probe_error_stops_recovery_before_resend() {
        let host = Fake {
            probe_error: Some("probe failed".into()),
            ..Fake::returning(None)
        };

        let error = recover_started_effect(
            EffectKind::CheckBeforeRedo,
            Uuid::new_v4(),
            &Value::Null,
            &host,
        )
        .await
        .expect_err("probe failure should propagate");

        assert_eq!(error, "probe failed");
        assert_eq!(host.probes.load(Ordering::SeqCst), 1);
        assert_eq!(host.sends.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn resend_error_is_returned() {
        let host = Fake {
            resend_error: Some("delivery failed".into()),
            ..Fake::returning(None)
        };

        let error = recover_started_effect(
            EffectKind::IdempotencyKey,
            Uuid::new_v4(),
            &Value::Null,
            &host,
        )
        .await
        .expect_err("delivery failure should propagate");

        assert_eq!(error, "delivery failed");
        assert_eq!(host.probes.load(Ordering::SeqCst), 0);
        assert_eq!(host.sends.load(Ordering::SeqCst), 1);
    }
}
