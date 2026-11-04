//! The only network code in rostr. HTTPS only, TLS verification always on,
//! requests limited to the URL prefixes the probes declare, bodies capped.

use super::{ProbeError, ProbeErrorKind, error};
use std::time::Duration;

/// Fetches one URL with a body size cap. Tests substitute fixtures.
pub trait Fetcher {
    fn get(&self, probe: &str, url: &str, max_bytes: u64) -> Result<Vec<u8>, ProbeError>;
}

pub struct HttpsFetcher {
    agent: ureq::Agent,
    allowed: Vec<String>,
}

impl HttpsFetcher {
    /// `allowed` holds the fixed URL prefixes from every probe's `SourceInfo`.
    pub fn new(allowed: Vec<String>) -> HttpsFetcher {
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .https_only(true)
            .tls_config(tls)
            .max_redirects(3)
            .timeout_global(Some(Duration::from_secs(120)))
            .user_agent(concat!(
                "rostr/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/kookyleo/rostr)"
            ))
            .build()
            .into();
        HttpsFetcher { agent, allowed }
    }
}

pub(crate) fn check_url(probe: &str, url: &str, allowed: &[String]) -> Result<(), ProbeError> {
    if !url.starts_with("https://") || !allowed.iter().any(|p| url.starts_with(p.as_str())) {
        return Err(error(
            probe,
            ProbeErrorKind::PolicyDenied,
            format!("URL is not a declared HTTPS source: {url}"),
        ));
    }
    Ok(())
}

impl Fetcher for HttpsFetcher {
    fn get(&self, probe: &str, url: &str, max_bytes: u64) -> Result<Vec<u8>, ProbeError> {
        check_url(probe, url, &self.allowed)?;
        let mut response = self.agent.get(url).call().map_err(|e| {
            let kind = match &e {
                ureq::Error::StatusCode(429) => ProbeErrorKind::EndpointRateLimited,
                // Server-side trouble (the dataset viewer answers 500 while busy).
                ureq::Error::StatusCode(500..=599) => ProbeErrorKind::NetworkUnavailable,
                ureq::Error::StatusCode(_) => ProbeErrorKind::InvalidData,
                ureq::Error::Timeout(_) => ProbeErrorKind::Timeout,
                _ => ProbeErrorKind::NetworkUnavailable,
            };
            error(probe, kind, format!("GET {url}: {e}"))
        })?;
        response
            .body_mut()
            .with_config()
            .limit(max_bytes)
            .read_to_vec()
            .map_err(|e| {
                error(
                    probe,
                    ProbeErrorKind::InvalidData,
                    format!("GET {url}: body rejected (limit {max_bytes} bytes): {e}"),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_declared_https_urls() {
        let allowed = vec!["https://epoch.ai/data/".to_string()];
        assert!(check_url("p", "https://epoch.ai/data/benchmark_data.zip", &allowed).is_ok());
        assert!(check_url("p", "http://epoch.ai/data/benchmark_data.zip", &allowed).is_err());
        assert!(check_url("p", "https://epoch.ai.evil/data/x", &allowed).is_err());
        assert!(check_url("p", "https://example.com/", &allowed).is_err());
    }
}
