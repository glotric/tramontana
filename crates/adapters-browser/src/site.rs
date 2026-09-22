//! Which addresses a task may open, and what a "site" is.
//!
//! A site is an origin: scheme, host and port. It is what a person allows,
//! so `https://example.com` and `http://example.com` are different sites, as
//! are `example.com` and `www.example.com`, exactly as the browser itself
//! separates them.

use std::net::IpAddr;

use url::{Host, Url};

/// The longest address a task may open. Longer ones are refused rather than
/// shown cut short in a question, since what is cut off could be anything.
pub(crate) const ADDRESS_LIMIT: usize = 2048;

/// Why an address is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// Not a web address.
    NotAnAddress,
    /// Not `http` or `https`: files, the browser's own pages, scripts.
    Scheme,
    /// A name and password written into the address.
    Credentials,
    /// Too long to show whole.
    TooLong,
}

impl Refusal {
    /// In words the model can act on.
    pub(crate) fn explain(self) -> &'static str {
        match self {
            Self::NotAnAddress => {
                "That is not a web address. Give a full address such as https://example.com/."
            }
            Self::Scheme => {
                "Only http and https addresses can be opened: not files, scripts or the browser's own pages."
            }
            Self::Credentials => {
                "Addresses with a name or password written into them are not opened."
            }
            Self::TooLong => {
                "That address is too long to show the person whole, so it is not opened."
            }
        }
    }
}

/// An address a task may be asked about opening.
pub(crate) fn parse(address: &str) -> Result<Url, Refusal> {
    if address.len() > ADDRESS_LIMIT {
        return Err(Refusal::TooLong);
    }
    let url = Url::parse(address.trim()).map_err(|_| Refusal::NotAnAddress)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Refusal::Scheme);
    }
    if url.host().is_none() {
        return Err(Refusal::NotAnAddress);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Refusal::Credentials);
    }
    Ok(url)
}

/// The site an address belongs to, or `None` for one that has no site a
/// person could allow, such as `about:blank`.
pub(crate) fn origin(address: &str) -> Option<String> {
    let url = Url::parse(address).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let origin = url.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

/// Whether an address is on this computer or a private network, which a
/// question says: a page there is not "the web", and may be something of the
/// person's own, such as a router.
pub(crate) fn is_local(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            name == "localhost" || name.ends_with(".localhost") || name.ends_with(".local")
        }
        Some(Host::Ipv4(address)) => is_private(IpAddr::V4(address)),
        Some(Host::Ipv6(address)) => is_private(IpAddr::V6(address)),
        None => false,
    }
}

fn is_private(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1])
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_private(IpAddr::V4(v4)))
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn only_web_addresses_without_credentials_are_opened() {
        assert!(parse("https://example.com/a?b=c").is_ok());
        assert!(parse("  http://127.0.0.1:8080/  ").is_ok());
        for (address, refusal) in [
            ("example.com", Refusal::NotAnAddress),
            ("file:///etc/passwd", Refusal::Scheme),
            ("javascript:alert(1)", Refusal::Scheme),
            ("chrome://settings", Refusal::Scheme),
            ("data:text/html,hi", Refusal::Scheme),
            ("about:blank", Refusal::Scheme),
            ("https://user:pw@example.com/", Refusal::Credentials),
            ("https://user@example.com/", Refusal::Credentials),
        ] {
            assert_eq!(parse(address).unwrap_err(), refusal, "{address}");
        }
        let long = format!("https://example.com/{}", "a".repeat(ADDRESS_LIMIT));
        assert_eq!(parse(&long).unwrap_err(), Refusal::TooLong);
    }

    #[test]
    fn a_site_is_the_scheme_host_and_port() {
        assert_eq!(
            origin("https://Example.com/path?q=1#f").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            origin("https://example.com:443/").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            origin("http://127.0.0.1:8080/x").as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_ne!(origin("http://example.com"), origin("https://example.com"));
        assert_ne!(
            origin("https://example.com"),
            origin("https://www.example.com")
        );
        // Internationalized names are compared as the browser sees them.
        assert_eq!(
            origin("https://bücher.example/").as_deref(),
            Some("https://xn--bcher-kva.example")
        );
        for none in ["about:blank", "chrome-error://chromewebdata/", "not a url"] {
            assert_eq!(origin(none), None, "{none}");
        }
    }

    #[test]
    fn addresses_on_this_computer_or_network_are_recognized() {
        for local in [
            "http://localhost:3000/",
            "http://127.0.0.1/",
            "http://192.168.1.1/",
            "http://10.0.0.5/",
            "http://[::1]/",
            "http://printer.local/",
            "http://100.64.0.1/",
        ] {
            assert!(is_local(&parse(local).unwrap()), "{local}");
        }
        for web in ["https://example.com/", "http://8.8.8.8/"] {
            assert!(!is_local(&parse(web).unwrap()), "{web}");
        }
    }
}
