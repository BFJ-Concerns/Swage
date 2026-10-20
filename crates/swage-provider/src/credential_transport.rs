//! Whether a provider's credential would travel in plaintext, and what the
//! provider's policy makes of that.
//!
//! The fact is fixed when the provider is built: a credential is present or
//! not, and the base URL's scheme and host do not change between requests.
//! So the judgement is made once, at construction, and the gateway acts on
//! it there — recording a warning, or building a provider that refuses every
//! request — rather than checking each request on its way out. A loopback
//! endpoint is never plaintext in this sense: the bytes do not leave the
//! host.

use std::net::IpAddr;

use swage_config::PlaintextCredentialPolicy;
use url::{Host, Url};

use crate::provider::ProviderConfig;

/// A credential that would be sent over plain `http://` to an address that
/// is not loopback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaintextExposure {
    /// The host the request would reach, as the base URL names it.
    pub host: String,
    /// The base URL the provider was configured with.
    pub base_url: String,
}

impl PlaintextExposure {
    /// Why a provider under the refuse policy sends nothing.
    pub fn refusal_reason(&self, provider_id: &str) -> String {
        format!(
            "provider '{provider_id}' refuses to send its credential in plaintext to {} \
             ({}); use an https:// base URL or a loopback address, or set this provider's \
             plaintext_credentials to \"warn\" or \"allow\"",
            self.host, self.base_url
        )
    }

    /// What the audit record says when a provider under the warn policy sends.
    pub fn warning(&self, provider_id: &str) -> String {
        format!(
            "provider '{provider_id}' sends its credential in plaintext to {} ({}); anyone \
             on the path can read it",
            self.host, self.base_url
        )
    }
}

/// What a provider's policy makes of its credential's transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaintextCredentialVerdict {
    /// Nothing to say: no credential, an encrypted route, a loopback route,
    /// or an exposure the policy allows.
    Send,
    /// The exposure is real and the policy sends after warning.
    SendWarned(PlaintextExposure),
    /// The exposure is real and the policy sends nothing.
    Refused(PlaintextExposure),
}

impl ProviderConfig {
    /// The credential's plaintext exposure, when there is one: a credential
    /// is configured and the base URL is `http://` to a host that is not
    /// loopback. A base URL that does not parse names no host a request
    /// could reach, so it exposes nothing.
    pub fn plaintext_exposure(&self) -> Option<PlaintextExposure> {
        self.api_key.as_ref()?;
        let url = Url::parse(&self.base_url).ok()?;
        if url.scheme() != "http" {
            return None;
        }
        let host = url.host()?;
        if is_loopback(&host) {
            return None;
        }
        Some(PlaintextExposure {
            host: host.to_string(),
            base_url: self.base_url.clone(),
        })
    }

    /// The policy's verdict on this configuration's credential transport.
    pub fn plaintext_credential_verdict(
        &self,
        policy: PlaintextCredentialPolicy,
    ) -> PlaintextCredentialVerdict {
        let Some(exposure) = self.plaintext_exposure() else {
            return PlaintextCredentialVerdict::Send;
        };
        match policy {
            PlaintextCredentialPolicy::Refuse => PlaintextCredentialVerdict::Refused(exposure),
            PlaintextCredentialPolicy::Warn => PlaintextCredentialVerdict::SendWarned(exposure),
            PlaintextCredentialPolicy::Allow => PlaintextCredentialVerdict::Send,
        }
    }
}

/// `localhost`, a name under it, or an address the kernel keeps on the host.
fn is_loopback(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(name) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            name == "localhost" || name.ends_with(".localhost")
        }
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => {
            address.is_loopback()
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| IpAddr::V4(mapped).is_loopback())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyed(base_url: &str) -> ProviderConfig {
        ProviderConfig::new("secret", "model", base_url)
    }

    #[test]
    fn a_credential_over_https_or_to_loopback_exposes_nothing() {
        for base_url in [
            "https://api.example.com/v1",
            "http://127.0.0.1:8080/v1",
            "http://127.5.5.5/v1",
            "http://localhost:11434",
            "http://LOCALHOST./v1",
            "http://models.localhost/v1",
            "http://[::1]:8080/v1",
            "http://[::ffff:127.0.0.1]:8080/v1",
        ] {
            assert_eq!(keyed(base_url).plaintext_exposure(), None, "{base_url}");
        }
    }

    #[test]
    fn a_credential_over_http_to_a_remote_host_is_exposed() {
        for (base_url, host) in [
            ("http://10.0.0.5:8080/v1", "10.0.0.5"),
            ("http://models.example.com/v1", "models.example.com"),
            ("http://[fd00::5]:8080/v1", "[fd00::5]"),
        ] {
            assert_eq!(
                keyed(base_url).plaintext_exposure(),
                Some(PlaintextExposure {
                    host: host.into(),
                    base_url: base_url.into(),
                }),
                "{base_url}"
            );
        }
    }

    #[test]
    fn no_credential_or_no_reachable_host_exposes_nothing() {
        assert_eq!(
            ProviderConfig::keyless("model", "http://models.example.com/v1").plaintext_exposure(),
            None
        );
        assert_eq!(keyed("not a url").plaintext_exposure(), None);
        assert_eq!(keyed("http://").plaintext_exposure(), None);
    }

    #[test]
    fn the_policy_decides_only_where_there_is_an_exposure() {
        let remote = keyed("http://models.example.com/v1");
        let exposure = remote.plaintext_exposure().unwrap();
        assert_eq!(
            remote.plaintext_credential_verdict(PlaintextCredentialPolicy::Refuse),
            PlaintextCredentialVerdict::Refused(exposure.clone())
        );
        assert_eq!(
            remote.plaintext_credential_verdict(PlaintextCredentialPolicy::Warn),
            PlaintextCredentialVerdict::SendWarned(exposure)
        );
        assert_eq!(
            remote.plaintext_credential_verdict(PlaintextCredentialPolicy::Allow),
            PlaintextCredentialVerdict::Send
        );

        let loopback = keyed("http://127.0.0.1:8080/v1");
        assert_eq!(
            loopback.plaintext_credential_verdict(PlaintextCredentialPolicy::Refuse),
            PlaintextCredentialVerdict::Send
        );
    }

    #[test]
    fn the_texts_name_the_provider_the_host_and_the_way_out() {
        let exposure = keyed("http://models.example.com/v1")
            .plaintext_exposure()
            .unwrap();
        let reason = exposure.refusal_reason("lan-models");
        assert!(reason.contains("'lan-models'"));
        assert!(reason.contains("models.example.com"));
        assert!(reason.contains("plaintext_credentials"));
        let warning = exposure.warning("lan-models");
        assert!(warning.contains("'lan-models'"));
        assert!(warning.contains("http://models.example.com/v1"));
        assert!(!reason.contains("secret") && !warning.contains("secret"));
    }
}
