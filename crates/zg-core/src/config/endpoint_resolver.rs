//! Connect-time endpoint validation for ureq agents.
//!
//! [`ValidatedEndpointResolver`] resolves a request hostname exactly
//! once and applies the SSRF IP policy (see `super::endpoint`) to the
//! addresses it hands to ureq's connector. ureq dials exactly those
//! addresses while retaining the original hostname for the `Host`
//! header and TLS SNI/certificate checks, so the check and the connect
//! can no longer observe different DNS answers (DNS rebinding / TOCTOU).
//! Every redirect hop re-enters the resolver, so rebound redirect
//! targets are rejected before any request body or credentials are sent.

use std::fmt;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::sync::mpsc;

use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

use super::endpoint::{is_blocked_hostname, is_blocked_ip};

/// Applies the IP policy to one already-resolved address set: empty sets
/// fail closed, and under the strict policy a single blocked address
/// rejects the whole set (a rebinding answer must not be dialed at all).
pub(super) fn check_resolved_addrs(
    addrs: Vec<SocketAddr>,
    allow_private_endpoint: bool,
) -> Option<Vec<SocketAddr>> {
    if addrs.is_empty() {
        return None;
    }
    if !allow_private_endpoint && addrs.iter().any(|addr| is_blocked_ip(&addr.ip())) {
        return None;
    }
    Some(addrs)
}

type LookupFn = dyn Fn(&str, u16) -> Option<Vec<SocketAddr>> + Send + Sync;

/// ureq resolver that validates the addresses it returns at connect time.
pub struct ValidatedEndpointResolver {
    allow_private_endpoint: bool,
    lookup: Arc<LookupFn>,
}

impl fmt::Debug for ValidatedEndpointResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ValidatedEndpointResolver")
            .field("allow_private_endpoint", &self.allow_private_endpoint)
            .finish_non_exhaustive()
    }
}

impl ValidatedEndpointResolver {
    /// Resolver enforcing the request-time endpoint policy; with
    /// `allow_private_endpoint` the operator opt-out skips the IP/hostname
    /// ranges but resolution still happens exactly once, here.
    #[must_use]
    pub fn new(allow_private_endpoint: bool) -> Self {
        Self {
            allow_private_endpoint,
            lookup: Arc::new(|host, port| {
                (host, port).to_socket_addrs().ok().map(Iterator::collect)
            }),
        }
    }

    #[cfg(test)]
    fn with_lookup(
        allow_private_endpoint: bool,
        lookup: impl Fn(&str, u16) -> Option<Vec<SocketAddr>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            allow_private_endpoint,
            lookup: Arc::new(lookup),
        }
    }
}

impl Resolver for ValidatedEndpointResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let scheme = uri.scheme_str().unwrap_or_default();
        if !matches!(scheme, "http" | "https") {
            return Err(ureq::Error::BadUri(format!("unknown scheme: {scheme}")));
        }
        let authority = uri.authority().ok_or(ureq::Error::HostNotFound)?;
        let raw_host = authority.host();
        let host = raw_host
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .unwrap_or(raw_host)
            .to_owned();
        let port = authority
            .port_u16()
            .unwrap_or(if scheme == "http" { 80 } else { 443 });
        let allow_private_endpoint = self.allow_private_endpoint;
        let lookup = Arc::clone(&self.lookup);
        let resolve = move || {
            if !allow_private_endpoint && is_blocked_hostname(&host) {
                return None;
            }
            if let Ok(ip) = host.parse::<IpAddr>() {
                return check_resolved_addrs(
                    vec![SocketAddr::new(ip, port)],
                    allow_private_endpoint,
                );
            }
            check_resolved_addrs(lookup(&host, port)?, allow_private_endpoint)
        };
        let addrs = if timeout.after.is_not_happening() {
            resolve()
        } else {
            let (tx, rx) = mpsc::sync_channel(1);
            std::thread::spawn(move || {
                let _ = tx.send(resolve());
            });
            match rx.recv_timeout(*timeout.after) {
                Ok(addrs) => addrs,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(ureq::Error::Timeout(timeout.reason));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => None,
            }
        };
        let addrs = addrs.ok_or(ureq::Error::HostNotFound)?;
        let mut result = self.empty();
        for addr in config.ip_family().keep_wanted(addrs.into_iter()).take(16) {
            result.push(addr);
        }
        if result.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(result)
    }
}

/// Builds a ureq agent whose connections can only dial addresses
/// validated by [`ValidatedEndpointResolver`] at connect time.
#[must_use]
pub fn agent_with_validated_resolver(
    config: ureq::config::Config,
    allow_private_endpoint: bool,
) -> ureq::Agent {
    ureq::Agent::with_parts(
        config,
        DefaultConnector::default(),
        ValidatedEndpointResolver::new(allow_private_endpoint),
    )
}

#[cfg(test)]
mod tests {
    use super::{ValidatedEndpointResolver, check_resolved_addrs};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[test]
    fn resolved_addr_set_with_any_blocked_ip_is_rejected() {
        let mixed = vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443),
        ];
        assert!(check_resolved_addrs(mixed.clone(), false).is_none());
        assert_eq!(check_resolved_addrs(mixed, true).map(|a| a.len()), Some(2));
        assert!(check_resolved_addrs(Vec::new(), false).is_none());
        assert!(check_resolved_addrs(Vec::new(), true).is_none());
    }

    #[test]
    fn resolver_rejects_rebound_loopback_at_connect_time() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc as channel;
        use std::time::Duration;
        use ureq::unversioned::transport::DefaultConnector;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind test listener");
        let port = listener.local_addr().expect("listener addr").port();
        let (tx, rx) = channel::channel::<String>();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let read = stream.read(&mut buf).unwrap_or(0);
                let _ =
                    tx.send(String::from_utf8_lossy(buf.get(..read).unwrap_or(&[])).into_owned());
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
            }
        });
        // Simulates the rebinding second answer: the name that passed a
        // public pre-check now resolves to loopback at connect time.
        let lookup = |_host: &str, lookup_port: u16| {
            Some(vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                lookup_port,
            )])
        };
        let agent = |allow_private: bool| {
            let config = ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .http_status_as_error(false)
                .build();
            ureq::Agent::with_parts(
                config,
                DefaultConnector::default(),
                ValidatedEndpointResolver::with_lookup(allow_private, lookup),
            )
        };
        let url = format!("http://rebind.test:{port}/");
        let strict = agent(false).get(&url).call();
        assert!(
            strict.is_err(),
            "rebound loopback must be blocked: {strict:?}"
        );
        assert!(
            rx.try_recv().is_err(),
            "blocked request must never reach the rebound server"
        );
        let mut response = agent(true).get(&url).call().expect("opt-out must connect");
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.body_mut().read_to_string().expect("body"), "ok");
        let request = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("server must receive the opt-out request");
        assert!(request.contains("rebind.test"), "{request}");
    }
}
